use anchor_lang::{
    prelude::Pubkey, solana_program::system_instruction, AccountDeserialize, InstructionData,
    ToAccountMetas,
};
use anchor_spl::{
    token_2022::{
        spl_token_2022::{
            extension::{
                default_account_state::DefaultAccountState,
                metadata_pointer::MetadataPointer,
                pausable::{PausableAccount, PausableConfig},
                BaseStateWithExtensions, ExtensionType, StateWithExtensions,
            },
            instruction::initialize_account3,
            state::{Account as TokenAccountState, AccountState, Mint as MintState},
        },
        ID as TOKEN_2022_ID,
    },
    token_2022_extensions::{
        spl_pod::optional_keys::OptionalNonZeroPubkey,
        spl_token_metadata_interface::state::TokenMetadata,
    },
};
use litesvm::{types::FailedTransactionMetadata, LiteSVM};
use solana_instruction::Instruction;
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use stablecoin::{
    constants::{CONFIG_SEED, MINT_SEED},
    errors::StablecoinError,
    state::MintConfig,
};

// CARGO_TARGET_TMPDIR is `<target>/tmp`, so the deploy directory is one level up.
const STABLECOIN_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/stablecoin.so"
));

const SYMBOL: [u8; 8] = *b"USDx\0\0\0\0";
const NAME: &str = "Portfolio USD";
const URI: &str = "https://example.com/usdx.json";
const SUPPLY_CAP: u64 = 250_000_000_000;
const DECIMALS: u8 = 6;

fn setup() -> (LiteSVM, Keypair) {
    let mut svm = LiteSVM::new();
    svm.add_program(stablecoin::id(), STABLECOIN_SO)
        .expect("failed to load stablecoin.so; run `anchor build` first");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000)
        .expect("airdrop to payer failed");

    (svm, payer)
}

fn mint_pda(symbol: &[u8; 8]) -> Pubkey {
    Pubkey::find_program_address(&[MINT_SEED, symbol], &stablecoin::id()).0
}

fn config_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CONFIG_SEED, mint.as_ref()], &stablecoin::id()).0
}

fn initialize_ix(
    payer: &Pubkey,
    symbol: [u8; 8],
    name: &str,
    uri: &str,
    decimals: u8,
    supply_cap: u64,
) -> Instruction {
    let mint = mint_pda(&symbol);
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::InitializeStablecoin {
            symbol,
            name: name.to_string(),
            uri: uri.to_string(),
            decimals,
            supply_cap,
        }
        .data(),
        stablecoin::accounts::InitializeStablecoin {
            payer: *payer,
            mint,
            config: config_pda(&mint),
            token_program: TOKEN_2022_ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn send(
    svm: &mut LiteSVM,
    payer: &Keypair,
    instructions: &[Instruction],
    extra_signers: &[&Keypair],
) -> Result<(), Box<FailedTransactionMetadata>> {
    let message =
        Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &svm.latest_blockhash());
    let mut signers = vec![payer];
    signers.extend_from_slice(extra_signers);
    let transaction = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &signers)
        .expect("failed to sign transaction");

    svm.send_transaction(transaction)
        .map(|_| ())
        .map_err(Box::new)
}

fn initialize_default() -> (LiteSVM, Keypair, Pubkey, Pubkey) {
    let (mut svm, payer) = setup();
    let instruction = initialize_ix(&payer.pubkey(), SYMBOL, NAME, URI, DECIMALS, SUPPLY_CAP);
    if let Err(failure) = send(&mut svm, &payer, &[instruction], &[]) {
        panic!(
            "initialize_stablecoin failed: {:?}\n{}",
            failure.err,
            failure.meta.logs.join("\n")
        );
    }

    let mint = mint_pda(&SYMBOL);
    let config = config_pda(&mint);
    (svm, payer, mint, config)
}

fn assert_fails_with(
    result: Result<(), Box<FailedTransactionMetadata>>,
    expected: StablecoinError,
) {
    let failure = result.expect_err("expected the transaction to fail");
    match failure.err {
        TransactionError::InstructionError(_, InstructionError::Custom(code)) => {
            assert_eq!(code, u32::from(expected), "unexpected custom error code");
        }
        other => panic!("expected a custom instruction error, got {other:?}"),
    }
}

fn some_key(key: Pubkey) -> OptionalNonZeroPubkey {
    OptionalNonZeroPubkey::try_from(Some(key)).expect("pubkey must be non-zero")
}

#[test]
fn initialize_stablecoin_creates_canonical_token_2022_mint() {
    let (svm, _payer, mint, config) = initialize_default();

    assert_eq!(
        mint,
        Pubkey::find_program_address(&[b"mint", &SYMBOL], &stablecoin::id()).0
    );
    assert_eq!(
        config,
        Pubkey::find_program_address(&[b"config", mint.as_ref()], &stablecoin::id()).0
    );

    let account = svm.get_account(&mint).expect("mint account missing");
    assert_eq!(
        account.owner, TOKEN_2022_ID,
        "mint must be owned by Token-2022"
    );
    assert!(
        account.lamports >= svm.minimum_balance_for_rent_exemption(account.data.len()),
        "mint must be rent exempt at its final length"
    );

    let state =
        StateWithExtensions::<MintState>::unpack(&account.data).expect("mint unpack failed");
    assert_eq!(state.base.decimals, DECIMALS);
    assert_eq!(state.base.supply, 0);
    assert_eq!(Option::from(state.base.mint_authority), Some(config));
    assert_eq!(Option::from(state.base.freeze_authority), Some(config));
}

#[test]
fn initialize_stablecoin_configures_compliance_extensions() {
    let (svm, _payer, mint, config) = initialize_default();

    let account = svm.get_account(&mint).expect("mint account missing");
    let state =
        StateWithExtensions::<MintState>::unpack(&account.data).expect("mint unpack failed");

    let default_state = state
        .get_extension::<DefaultAccountState>()
        .expect("default account state extension missing");
    assert_eq!(default_state.state, u8::from(AccountState::Frozen));

    let pausable = state
        .get_extension::<PausableConfig>()
        .expect("pausable extension missing");
    assert_eq!(pausable.authority, some_key(config));
    assert!(!bool::from(pausable.paused), "mint must start unpaused");

    let pointer = state
        .get_extension::<MetadataPointer>()
        .expect("metadata pointer extension missing");
    assert_eq!(pointer.authority, some_key(config));
    assert_eq!(pointer.metadata_address, some_key(mint));
}

#[test]
fn initialize_stablecoin_writes_token_metadata() {
    let (svm, _payer, mint, config) = initialize_default();

    let account = svm.get_account(&mint).expect("mint account missing");
    let state =
        StateWithExtensions::<MintState>::unpack(&account.data).expect("mint unpack failed");
    let metadata = state
        .get_variable_len_extension::<TokenMetadata>()
        .expect("token metadata missing");

    assert_eq!(metadata.name, NAME);
    assert_eq!(metadata.symbol, "USDx");
    assert_eq!(metadata.uri, URI);
    assert_eq!(metadata.mint, mint);
    assert_eq!(metadata.update_authority, some_key(config));
    assert!(metadata.additional_metadata.is_empty());
}

#[test]
fn initialize_stablecoin_populates_mint_config() {
    let (svm, payer, mint, config) = initialize_default();

    let account = svm.get_account(&config).expect("config account missing");
    assert_eq!(account.owner, stablecoin::id());

    let state = MintConfig::try_deserialize(&mut account.data.as_slice())
        .expect("config deserialization failed");
    assert_eq!(state.admin, payer.pubkey());
    assert_eq!(state.compliance_authority, payer.pubkey());
    assert_eq!(state.pending_admin, None);
    assert_eq!(state.mint, mint);
    assert_eq!(state.symbol, SYMBOL);
    assert_eq!(state.decimals, DECIMALS);
    assert_eq!(state.supply_cap, SUPPLY_CAP);
    assert_eq!(state.total_minted, 0);
    assert_eq!(state.total_burned, 0);
    assert!(!state.paused);
    assert_eq!(
        Pubkey::create_program_address(
            &[CONFIG_SEED, mint.as_ref(), &[state.bump]],
            &stablecoin::id()
        ),
        Ok(config)
    );
    assert_eq!(
        Pubkey::create_program_address(
            &[MINT_SEED, &SYMBOL, &[state.mint_bump]],
            &stablecoin::id()
        ),
        Ok(mint)
    );
}

#[test]
fn new_token_account_starts_frozen_with_pausable_extension() {
    let (mut svm, payer, mint, _config) = initialize_default();

    let token_account = Keypair::new();
    let space = ExtensionType::try_calculate_account_len::<TokenAccountState>(&[
        ExtensionType::PausableAccount,
    ])
    .expect("token account length calculation failed");
    let create = system_instruction::create_account(
        &payer.pubkey(),
        &token_account.pubkey(),
        svm.minimum_balance_for_rent_exemption(space),
        space as u64,
        &TOKEN_2022_ID,
    );
    let initialize = initialize_account3(
        &TOKEN_2022_ID,
        &token_account.pubkey(),
        &mint,
        &payer.pubkey(),
    )
    .expect("initialize_account3 build failed");

    send(&mut svm, &payer, &[create, initialize], &[&token_account])
        .expect("token account creation failed");

    let account = svm
        .get_account(&token_account.pubkey())
        .expect("token account missing");
    let state =
        StateWithExtensions::<TokenAccountState>::unpack(&account.data).expect("unpack failed");
    assert_eq!(state.base.state, AccountState::Frozen);
    state
        .get_extension::<PausableAccount>()
        .expect("pausable account extension missing");
}

#[test]
fn reinitialization_fails() {
    let (mut svm, payer, _mint, _config) = initialize_default();

    svm.expire_blockhash();
    let instruction = initialize_ix(&payer.pubkey(), SYMBOL, NAME, URI, DECIMALS, SUPPLY_CAP);
    let failure = send(&mut svm, &payer, &[instruction], &[])
        .expect_err("reinitializing an existing stablecoin must fail");
    assert_eq!(
        failure.err,
        TransactionError::InstructionError(0, InstructionError::Custom(0)),
        "expected the system program to reject allocating accounts that already exist"
    );
}

#[test]
fn unsupported_decimals_fails() {
    let (mut svm, payer) = setup();

    let instruction = initialize_ix(&payer.pubkey(), SYMBOL, NAME, URI, 9, SUPPLY_CAP);
    assert_fails_with(
        send(&mut svm, &payer, &[instruction], &[]),
        StablecoinError::UnsupportedDecimals,
    );
}

#[test]
fn zero_supply_cap_fails() {
    let (mut svm, payer) = setup();

    let instruction = initialize_ix(&payer.pubkey(), SYMBOL, NAME, URI, DECIMALS, 0);
    assert_fails_with(
        send(&mut svm, &payer, &[instruction], &[]),
        StablecoinError::ZeroSupplyCap,
    );
}

#[test]
fn supply_cap_above_maximum_fails() {
    let (mut svm, payer) = setup();

    let instruction = initialize_ix(
        &payer.pubkey(),
        SYMBOL,
        NAME,
        URI,
        DECIMALS,
        stablecoin::constants::MAX_SUPPLY_CAP + 1,
    );
    assert_fails_with(
        send(&mut svm, &payer, &[instruction], &[]),
        StablecoinError::SupplyCapTooLarge,
    );
}

#[test]
fn invalid_symbols_fail() {
    for symbol in [[0u8; 8], *b"US\0Dx\0\0\0", *b"USD-x\0\0\0"] {
        let (mut svm, payer) = setup();
        let instruction = initialize_ix(&payer.pubkey(), symbol, NAME, URI, DECIMALS, SUPPLY_CAP);
        assert_fails_with(
            send(&mut svm, &payer, &[instruction], &[]),
            StablecoinError::InvalidSymbol,
        );
    }
}

#[test]
fn unpadded_symbol_is_accepted() {
    let (mut svm, payer) = setup();
    let symbol = *b"USDxEURx";

    let instruction = initialize_ix(&payer.pubkey(), symbol, NAME, URI, DECIMALS, SUPPLY_CAP);
    send(&mut svm, &payer, &[instruction], &[]).expect("a fully populated symbol must be valid");

    let mint = mint_pda(&symbol);
    let account = svm.get_account(&mint).expect("mint account missing");
    let state =
        StateWithExtensions::<MintState>::unpack(&account.data).expect("mint unpack failed");
    let metadata = state
        .get_variable_len_extension::<TokenMetadata>()
        .expect("token metadata missing");
    assert_eq!(metadata.symbol, "USDxEURx");
}

#[test]
fn invalid_name_fails() {
    for name in [
        String::new(),
        "n".repeat(stablecoin::constants::MAX_NAME_LEN + 1),
    ] {
        let (mut svm, payer) = setup();
        let instruction = initialize_ix(&payer.pubkey(), SYMBOL, &name, URI, DECIMALS, SUPPLY_CAP);
        assert_fails_with(
            send(&mut svm, &payer, &[instruction], &[]),
            StablecoinError::InvalidName,
        );
    }
}

#[test]
fn invalid_uri_fails() {
    for uri in [
        String::new(),
        "u".repeat(stablecoin::constants::MAX_URI_LEN + 1),
    ] {
        let (mut svm, payer) = setup();
        let instruction = initialize_ix(&payer.pubkey(), SYMBOL, NAME, &uri, DECIMALS, SUPPLY_CAP);
        assert_fails_with(
            send(&mut svm, &payer, &[instruction], &[]),
            StablecoinError::InvalidUri,
        );
    }
}
