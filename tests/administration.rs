mod support;

use anchor_lang::{
    error::ErrorCode as AnchorError, prelude::Pubkey, solana_program::system_instruction,
    AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
};
use anchor_spl::{
    token_2022::{
        spl_token_2022::{
            error::TokenError,
            extension::{
                pausable::PausableConfig, BaseStateWithExtensions, ExtensionType,
                StateWithExtensions,
            },
            instruction::{initialize_account3, transfer_checked},
            state::{Account as TokenAccountState, AccountState, Mint as MintState},
        },
        ID as TOKEN_2022_ID,
    },
    token_2022_extensions::spl_pod::optional_keys::OptionalNonZeroPubkey,
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
    constants::{CONFIG_SEED, MAX_SUPPLY_CAP, MINTER_SEED, MINT_SEED, POLICY_SEED},
    errors::StablecoinError,
    events::{AdminAccepted, ConfigUpdated, PauseChanged},
    state::{MintConfig, PolicyStatus},
};

// CARGO_TARGET_TMPDIR is `<target>/tmp`, so the deploy directory is one level up.
const STABLECOIN_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/stablecoin.so"
));

const SYMBOL: [u8; 8] = *b"USDx\0\0\0\0";
const ALT_SYMBOL: [u8; 8] = *b"EURx\0\0\0\0";
const NAME: &str = "Portfolio USD";
const URI: &str = "https://example.com/usdx.json";
const DECIMALS: u8 = 6;
const SUPPLY_CAP: u64 = 1_000_000_000_000;
const ALLOWANCE: u64 = 500_000_000;

type TxResult = Result<(), Box<FailedTransactionMetadata>>;

fn mint_pda(symbol: &[u8; 8]) -> Pubkey {
    Pubkey::find_program_address(&[MINT_SEED, symbol], &stablecoin::id()).0
}

fn config_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CONFIG_SEED, mint.as_ref()], &stablecoin::id()).0
}

fn minter_pda(config: &Pubkey, authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[MINTER_SEED, config.as_ref(), authority.as_ref()],
        &stablecoin::id(),
    )
    .0
}

fn policy_pda(mint: &Pubkey, token_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POLICY_SEED, mint.as_ref(), token_account.as_ref()],
        &stablecoin::id(),
    )
    .0
}

fn initialize_ix(payer: &Pubkey, symbol: [u8; 8], supply_cap: u64) -> Instruction {
    let mint = mint_pda(&symbol);
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::InitializeStablecoin {
            symbol,
            name: NAME.to_string(),
            uri: URI.to_string(),
            decimals: DECIMALS,
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

fn set_pause_ix(authority: &Pubkey, mint: &Pubkey, paused: bool) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::SetPause { paused }.data(),
        stablecoin::accounts::SetPause {
            authority: *authority,
            mint: *mint,
            config: config_pda(mint),
            token_program: TOKEN_2022_ID,
        }
        .to_account_metas(None),
    )
}

fn update_config_ix(
    admin: &Pubkey,
    mint: &Pubkey,
    supply_cap: Option<u64>,
    compliance_authority: Option<Pubkey>,
    pending_admin: Option<Pubkey>,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::UpdateConfig {
            supply_cap,
            compliance_authority,
            pending_admin,
        }
        .data(),
        stablecoin::accounts::UpdateConfig {
            admin: *admin,
            mint: *mint,
            config: config_pda(mint),
        }
        .to_account_metas(None),
    )
}

fn accept_admin_ix(pending_admin: &Pubkey, config: &Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::AcceptAdmin {}.data(),
        stablecoin::accounts::AcceptAdmin {
            pending_admin: *pending_admin,
            config: *config,
        }
        .to_account_metas(None),
    )
}

fn set_wallet_policy_ix(
    compliance_authority: &Pubkey,
    mint: &Pubkey,
    token_account: &Pubkey,
    status: PolicyStatus,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::SetWalletPolicy { status }.data(),
        stablecoin::accounts::SetWalletPolicy {
            compliance_authority: *compliance_authority,
            mint: *mint,
            config: config_pda(mint),
            token_account: *token_account,
            wallet_policy: policy_pda(mint, token_account),
            token_program: TOKEN_2022_ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
    )
}

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    mint: Pubkey,
    config: Pubkey,
    logs: Vec<String>,
}

impl Env {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(stablecoin::id(), STABLECOIN_SO)
            .expect("failed to load stablecoin.so; run `anchor build` first");

        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 100_000_000_000)
            .expect("airdrop to admin failed");

        let mint = mint_pda(&SYMBOL);
        let mut env = Self {
            svm,
            admin,
            mint,
            config: config_pda(&mint),
            logs: Vec::new(),
        };
        let instruction = initialize_ix(&env.admin.pubkey(), SYMBOL, SUPPLY_CAP);
        env.send(&[instruction], &[])
            .expect("initialization failed");
        env
    }

    fn send(&mut self, instructions: &[Instruction], extra_signers: &[&Keypair]) -> TxResult {
        self.svm.expire_blockhash();
        let message = Message::new_with_blockhash(
            instructions,
            Some(&self.admin.pubkey()),
            &self.svm.latest_blockhash(),
        );
        let mut signers = vec![&self.admin];
        for signer in extra_signers {
            if !signers
                .iter()
                .any(|existing| existing.pubkey() == signer.pubkey())
            {
                signers.push(signer);
            }
        }
        let transaction =
            VersionedTransaction::try_new(VersionedMessage::Legacy(message), &signers)
                .expect("failed to sign transaction");

        self.logs.clear();
        match self.svm.send_transaction(transaction) {
            Ok(meta) => {
                self.logs = meta.logs;
                Ok(())
            }
            Err(failure) => {
                self.logs = failure.meta.logs.clone();
                Err(Box::new(failure))
            }
        }
    }

    fn events<E: anchor_lang::Discriminator + anchor_lang::AnchorDeserialize>(&self) -> Vec<E> {
        support::decode_events(&self.logs)
    }

    fn funded_keypair(&mut self) -> Keypair {
        let keypair = Keypair::new();
        self.svm
            .airdrop(&keypair.pubkey(), 10_000_000_000)
            .expect("airdrop failed");
        keypair
    }

    fn init_alt_stablecoin(&mut self) -> Pubkey {
        let mint = mint_pda(&ALT_SYMBOL);
        let instruction = initialize_ix(&self.admin.pubkey(), ALT_SYMBOL, SUPPLY_CAP);
        self.send(&[instruction], &[])
            .expect("alternate initialization failed");
        mint
    }

    fn create_token_account(&mut self, owner: &Pubkey) -> Pubkey {
        let mint = self.mint;
        let account = Keypair::new();
        let space = ExtensionType::try_calculate_account_len::<TokenAccountState>(&[
            ExtensionType::PausableAccount,
        ])
        .expect("token account length calculation failed");
        let create = system_instruction::create_account(
            &self.admin.pubkey(),
            &account.pubkey(),
            self.svm.minimum_balance_for_rent_exemption(space),
            space as u64,
            &TOKEN_2022_ID,
        );
        let initialize = initialize_account3(&TOKEN_2022_ID, &account.pubkey(), &mint, owner)
            .expect("initialize_account3 build failed");
        self.send(&[create, initialize], &[&account])
            .expect("token account creation failed");
        account.pubkey()
    }

    fn pause_as(&mut self, authority: &Keypair, paused: bool) -> TxResult {
        let instruction = set_pause_ix(&authority.pubkey(), &self.mint, paused);
        self.send(&[instruction], &[authority])
    }

    fn pause(&mut self, paused: bool) -> TxResult {
        let admin = self.admin.insecure_clone();
        self.pause_as(&admin, paused)
    }

    fn update_as(
        &mut self,
        admin: &Keypair,
        supply_cap: Option<u64>,
        compliance_authority: Option<Pubkey>,
        pending_admin: Option<Pubkey>,
    ) -> TxResult {
        let instruction = update_config_ix(
            &admin.pubkey(),
            &self.mint,
            supply_cap,
            compliance_authority,
            pending_admin,
        );
        self.send(&[instruction], &[admin])
    }

    fn update(
        &mut self,
        supply_cap: Option<u64>,
        compliance_authority: Option<Pubkey>,
        pending_admin: Option<Pubkey>,
    ) -> TxResult {
        let admin = self.admin.insecure_clone();
        self.update_as(&admin, supply_cap, compliance_authority, pending_admin)
    }

    fn accept_admin(&mut self, pending: &Keypair) -> TxResult {
        let instruction = accept_admin_ix(&pending.pubkey(), &self.config);
        self.send(&[instruction], &[pending])
    }

    fn set_policy_as(
        &mut self,
        authority: &Keypair,
        token_account: &Pubkey,
        status: PolicyStatus,
    ) -> TxResult {
        let instruction =
            set_wallet_policy_ix(&authority.pubkey(), &self.mint, token_account, status);
        self.send(&[instruction], &[authority])
    }

    fn set_policy(&mut self, token_account: &Pubkey, status: PolicyStatus) -> TxResult {
        let admin = self.admin.insecure_clone();
        self.set_policy_as(&admin, token_account, status)
    }

    fn grant_as(&mut self, admin: &Keypair, authority: &Pubkey, allowance: u64) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::GrantMinter { allowance }.data(),
            stablecoin::accounts::GrantMinter {
                admin: admin.pubkey(),
                config: self.config,
                authority: *authority,
                minter_role: minter_pda(&self.config, authority),
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[admin])
    }

    fn revoke_as(&mut self, admin: &Keypair, authority: &Pubkey) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::RevokeMinter {}.data(),
            stablecoin::accounts::RevokeMinter {
                admin: admin.pubkey(),
                config: self.config,
                authority: *authority,
                minter_role: minter_pda(&self.config, authority),
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[admin])
    }

    fn mint_to(&mut self, minter: &Keypair, destination: &Pubkey, amount: u64) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::MintTo { amount }.data(),
            stablecoin::accounts::MintTo {
                minter: minter.pubkey(),
                mint: self.mint,
                config: self.config,
                minter_role: minter_pda(&self.config, &minter.pubkey()),
                destination: *destination,
                wallet_policy: policy_pda(&self.mint, destination),
                token_program: TOKEN_2022_ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[minter])
    }

    fn burn(&mut self, owner: &Keypair, source: &Pubkey, amount: u64) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::Burn { amount }.data(),
            stablecoin::accounts::Burn {
                owner: owner.pubkey(),
                mint: self.mint,
                config: self.config,
                source: *source,
                token_program: TOKEN_2022_ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[owner])
    }

    /// Sets up an allowed token account funded with `amount` tokens by a fresh minter.
    fn funded_account(&mut self, amount: u64) -> (Keypair, Pubkey) {
        let owner = self.funded_keypair();
        let token_account = self.create_token_account(&owner.pubkey());
        self.set_policy(&token_account, PolicyStatus::Allowed)
            .expect("allowing must succeed");

        let minter = self.funded_keypair();
        let admin = self.admin.insecure_clone();
        self.grant_as(&admin, &minter.pubkey(), ALLOWANCE)
            .expect("granting must succeed");
        self.mint_to(&minter, &token_account, amount)
            .expect("minting must succeed");
        (owner, token_account)
    }

    fn patch_config(&mut self, patch: impl FnOnce(&mut MintConfig)) {
        let mut account = self.svm.get_account(&self.config).expect("config missing");
        let mut config = MintConfig::try_deserialize(&mut account.data.as_slice())
            .expect("config deserialize failed");
        patch(&mut config);

        let mut data = Vec::new();
        config
            .try_serialize(&mut data)
            .expect("config serialize failed");
        data.resize(account.data.len(), 0);
        account.data = data;
        self.svm
            .set_account(self.config, account)
            .expect("set_account failed");
    }

    fn config_state(&self) -> MintConfig {
        self.config_state_of(&self.config)
    }

    fn config_state_of(&self, config: &Pubkey) -> MintConfig {
        let account = self.svm.get_account(config).expect("config missing");
        MintConfig::try_deserialize(&mut account.data.as_slice())
            .expect("config deserialize failed")
    }

    fn pausable(&self, mint: &Pubkey) -> PausableConfig {
        let account = self.svm.get_account(mint).expect("mint missing");
        let state = StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .get_extension::<PausableConfig>()
            .copied()
            .expect("pausable extension missing");
        state
    }

    fn extension_paused(&self, mint: &Pubkey) -> bool {
        bool::from(self.pausable(mint).paused)
    }

    fn supply(&self) -> u64 {
        let account = self.svm.get_account(&self.mint).expect("mint missing");
        StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .base
            .supply
    }

    fn is_frozen(&self, token_account: &Pubkey) -> bool {
        let account = self
            .svm
            .get_account(token_account)
            .expect("token account missing");
        StateWithExtensions::<TokenAccountState>::unpack(&account.data)
            .expect("token account unpack failed")
            .base
            .state
            == AccountState::Frozen
    }

    fn assert_pause_states_agree(&self, expected: bool) {
        assert_eq!(self.extension_paused(&self.mint), expected);
        assert_eq!(self.config_state().paused, expected);
    }
}

fn custom_code(result: TxResult) -> u32 {
    let failure = result.expect_err("expected the transaction to fail");
    match failure.err {
        TransactionError::InstructionError(_, InstructionError::Custom(code)) => code,
        ref other => panic!(
            "expected a custom instruction error, got {other:?}\n{}",
            failure.meta.logs.join("\n")
        ),
    }
}

fn expect_error(result: TxResult, expected: StablecoinError) {
    assert_eq!(custom_code(result), u32::from(expected));
}

fn expect_anchor_error(result: TxResult, expected: AnchorError) {
    assert_eq!(custom_code(result), u32::from(expected));
}

fn expect_token_error(result: TxResult, expected: TokenError) {
    assert_eq!(custom_code(result), expected as u32);
}

#[test]
fn admin_can_pause_and_resume() {
    let mut env = Env::new();
    env.assert_pause_states_agree(false);

    env.pause(true).expect("admin pause must succeed");
    env.assert_pause_states_agree(true);

    env.pause(false).expect("admin resume must succeed");
    env.assert_pause_states_agree(false);
}

#[test]
fn compliance_authority_can_pause_and_resume() {
    let mut env = Env::new();
    let compliance = env.funded_keypair();
    env.update(None, Some(compliance.pubkey()), None)
        .expect("rotating compliance authority must succeed");

    env.pause_as(&compliance, true)
        .expect("compliance pause must succeed");
    env.assert_pause_states_agree(true);

    env.pause_as(&compliance, false)
        .expect("compliance resume must succeed");
    env.assert_pause_states_agree(false);
}

#[test]
fn pause_authority_remains_the_config_pda() {
    let mut env = Env::new();
    let expected = OptionalNonZeroPubkey::try_from(Some(env.config)).expect("non-zero config");
    assert_eq!(env.pausable(&env.mint).authority, expected);

    env.pause(true).expect("pause must succeed");
    assert_eq!(env.pausable(&env.mint).authority, expected);

    env.pause(false).expect("resume must succeed");
    assert_eq!(env.pausable(&env.mint).authority, expected);
}

#[test]
fn unauthorized_signer_cannot_pause_or_resume() {
    let mut env = Env::new();
    let intruder = env.funded_keypair();

    expect_error(env.pause_as(&intruder, true), StablecoinError::Unauthorized);
    env.assert_pause_states_agree(false);

    env.pause(true).expect("admin pause must succeed");
    expect_error(
        env.pause_as(&intruder, false),
        StablecoinError::Unauthorized,
    );
    env.assert_pause_states_agree(true);
}

#[test]
fn double_pause_and_double_resume_fail_without_changing_state() {
    let mut env = Env::new();

    expect_error(env.pause(false), StablecoinError::PauseStateUnchanged);
    env.assert_pause_states_agree(false);

    env.pause(true).expect("pause must succeed");
    expect_error(env.pause(true), StablecoinError::PauseStateUnchanged);
    env.assert_pause_states_agree(true);
}

#[test]
fn direct_transfer_fails_while_paused() {
    let mut env = Env::new();
    let (sender, source) = env.funded_account(10_000);
    let receiver = env.funded_keypair();
    let destination = env.create_token_account(&receiver.pubkey());
    env.set_policy(&destination, PolicyStatus::Allowed)
        .expect("allowing must succeed");

    let build = |from: &Pubkey, to: &Pubkey, authority: &Pubkey, mint: &Pubkey| {
        transfer_checked(
            &TOKEN_2022_ID,
            from,
            mint,
            to,
            authority,
            &[],
            1_000,
            DECIMALS,
        )
        .expect("transfer_checked build failed")
    };

    let before = build(&source, &destination, &sender.pubkey(), &env.mint);
    env.send(&[before], &[&sender])
        .expect("transfers must work before pausing");

    env.pause(true).expect("pause must succeed");
    let after = build(&source, &destination, &sender.pubkey(), &env.mint);
    expect_token_error(env.send(&[after], &[&sender]), TokenError::MintPaused);
}

#[test]
fn program_mint_and_burn_fail_while_paused() {
    let mut env = Env::new();
    let (owner, token_account) = env.funded_account(10_000);
    let minter = env.funded_keypair();
    let admin = env.admin.insecure_clone();
    env.grant_as(&admin, &minter.pubkey(), ALLOWANCE)
        .expect("granting must succeed");

    env.pause(true).expect("pause must succeed");

    expect_error(
        env.mint_to(&minter, &token_account, 1),
        StablecoinError::ProtocolPaused,
    );
    expect_error(
        env.burn(&owner, &token_account, 1),
        StablecoinError::ProtocolPaused,
    );
    assert_eq!(env.supply(), 10_000);
}

#[test]
fn policy_changes_still_work_while_paused() {
    let mut env = Env::new();
    let (_owner, token_account) = env.funded_account(10_000);
    env.pause(true).expect("pause must succeed");

    env.set_policy(&token_account, PolicyStatus::Blocked)
        .expect("freezing must work while paused");
    assert!(env.is_frozen(&token_account));

    env.set_policy(&token_account, PolicyStatus::Allowed)
        .expect("thawing must work while paused");
    assert!(!env.is_frozen(&token_account));
    env.assert_pause_states_agree(true);
}

#[test]
fn pausing_one_stablecoin_does_not_pause_another() {
    let mut env = Env::new();
    let alt_mint = env.init_alt_stablecoin();

    env.pause(true).expect("pause must succeed");

    env.assert_pause_states_agree(true);
    assert!(!env.extension_paused(&alt_mint));
    assert!(!env.config_state_of(&config_pda(&alt_mint)).paused);
}

#[test]
fn corrupted_mirror_state_reports_drift_without_changing_state() {
    let mut env = Env::new();
    env.patch_config(|config| config.paused = true);

    expect_error(env.pause(false), StablecoinError::PauseStateDrift);
    expect_error(env.pause(true), StablecoinError::PauseStateDrift);

    assert!(!env.extension_paused(&env.mint));
    assert!(env.config_state().paused);
}

#[test]
fn supply_cap_can_be_increased() {
    let mut env = Env::new();
    let raised = SUPPLY_CAP * 2;

    env.update(Some(raised), None, None)
        .expect("raising the cap must succeed");
    assert_eq!(env.config_state().supply_cap, raised);
}

#[test]
fn supply_cap_can_be_lowered_above_and_exactly_to_current_supply() {
    let mut env = Env::new();
    let (_owner, _token_account) = env.funded_account(10_000);

    env.update(Some(50_000), None, None)
        .expect("lowering above supply must succeed");
    assert_eq!(env.config_state().supply_cap, 50_000);

    env.update(Some(10_000), None, None)
        .expect("lowering to exactly the supply must succeed");
    assert_eq!(env.config_state().supply_cap, 10_000);
}

#[test]
fn invalid_supply_caps_are_rejected() {
    let mut env = Env::new();
    let (_owner, _token_account) = env.funded_account(10_000);

    expect_error(
        env.update(Some(0), None, None),
        StablecoinError::ZeroSupplyCap,
    );
    expect_error(
        env.update(Some(MAX_SUPPLY_CAP + 1), None, None),
        StablecoinError::SupplyCapTooLarge,
    );
    expect_error(
        env.update(Some(9_999), None, None),
        StablecoinError::SupplyCapBelowSupply,
    );
    assert_eq!(env.config_state().supply_cap, SUPPLY_CAP);
}

#[test]
fn broken_counter_invariant_blocks_supply_cap_changes() {
    let mut env = Env::new();
    let (_owner, _token_account) = env.funded_account(10_000);
    env.patch_config(|config| config.total_minted += 1);

    expect_error(
        env.update(Some(50_000), None, None),
        StablecoinError::CounterInvariantViolation,
    );
}

#[test]
fn compliance_authority_rotation_transfers_policy_and_pause_access() {
    let mut env = Env::new();
    let (_owner, token_account) = env.funded_account(10_000);

    // The compliance authority is rotated away from the admin first, so the later
    // assertions isolate the compliance path from the admin path.
    let old = env.funded_keypair();
    env.update(None, Some(old.pubkey()), None)
        .expect("first rotation must succeed");
    env.set_policy_as(&old, &token_account, PolicyStatus::Blocked)
        .expect("the first authority must control policy");
    env.pause_as(&old, true)
        .expect("the first authority must control pause");
    env.pause_as(&old, false).expect("resume must succeed");

    let new = env.funded_keypair();
    env.update(None, Some(new.pubkey()), None)
        .expect("second rotation must succeed");
    assert_eq!(env.config_state().compliance_authority, new.pubkey());

    expect_error(
        env.set_policy_as(&old, &token_account, PolicyStatus::Allowed),
        StablecoinError::Unauthorized,
    );
    expect_error(env.pause_as(&old, true), StablecoinError::Unauthorized);

    env.set_policy_as(&new, &token_account, PolicyStatus::Allowed)
        .expect("the new authority must control policy");
    assert!(!env.is_frozen(&token_account));
    env.pause_as(&new, true)
        .expect("the new authority must control pause");
    env.assert_pause_states_agree(true);
}

#[test]
fn invalid_and_empty_updates_are_rejected() {
    let mut env = Env::new();

    expect_error(
        env.update(None, Some(Pubkey::default()), None),
        StablecoinError::InvalidAuthority,
    );
    expect_error(
        env.update(None, None, None),
        StablecoinError::NoConfigChange,
    );
}

#[test]
fn config_updates_work_while_paused() {
    let mut env = Env::new();
    env.pause(true).expect("pause must succeed");

    let compliance = env.funded_keypair();
    let nominee = env.funded_keypair();
    env.update(
        Some(2_000),
        Some(compliance.pubkey()),
        Some(nominee.pubkey()),
    )
    .expect("config updates must work while paused");

    let config = env.config_state();
    assert_eq!(config.supply_cap, 2_000);
    assert_eq!(config.compliance_authority, compliance.pubkey());
    assert_eq!(config.pending_admin, Some(nominee.pubkey()));
    assert!(config.paused);
}

#[test]
fn updates_leave_unrelated_fields_untouched() {
    let mut env = Env::new();
    let (_owner, _token_account) = env.funded_account(10_000);
    let before = env.config_state();

    env.update(Some(777_777), None, None)
        .expect("update must succeed");

    let after = env.config_state();
    assert_eq!(after.supply_cap, 777_777);
    assert_eq!(after.admin, before.admin);
    assert_eq!(after.pending_admin, before.pending_admin);
    assert_eq!(after.compliance_authority, before.compliance_authority);
    assert_eq!(after.mint, before.mint);
    assert_eq!(after.symbol, before.symbol);
    assert_eq!(after.decimals, before.decimals);
    assert_eq!(after.total_minted, before.total_minted);
    assert_eq!(after.total_burned, before.total_burned);
    assert_eq!(after.paused, before.paused);
    assert_eq!(after.bump, before.bump);
    assert_eq!(after.mint_bump, before.mint_bump);
}

#[test]
fn unauthorized_config_update_is_rejected() {
    let mut env = Env::new();
    let intruder = env.funded_keypair();

    expect_error(
        env.update_as(&intruder, Some(1_000), None, None),
        StablecoinError::Unauthorized,
    );
}

#[test]
fn nomination_does_not_change_the_current_admin() {
    let mut env = Env::new();
    let nominee = env.funded_keypair();

    env.update(None, None, Some(nominee.pubkey()))
        .expect("nomination must succeed");

    let config = env.config_state();
    assert_eq!(config.admin, env.admin.pubkey());
    assert_eq!(config.pending_admin, Some(nominee.pubkey()));
}

#[test]
fn only_the_pending_admin_can_accept() {
    let mut env = Env::new();
    let nominee = env.funded_keypair();
    let intruder = env.funded_keypair();
    env.update(None, None, Some(nominee.pubkey()))
        .expect("nomination must succeed");

    expect_error(env.accept_admin(&intruder), StablecoinError::NoPendingAdmin);
    assert_eq!(env.config_state().admin, env.admin.pubkey());

    env.accept_admin(&nominee).expect("acceptance must succeed");
    let config = env.config_state();
    assert_eq!(config.admin, nominee.pubkey());
    assert_eq!(config.pending_admin, None);

    expect_error(env.accept_admin(&nominee), StablecoinError::NoPendingAdmin);
}

#[test]
fn admin_rotation_transfers_every_admin_permission() {
    let mut env = Env::new();
    let old = env.admin.insecure_clone();
    let new = env.funded_keypair();
    let minter = env.funded_keypair();

    // Admin rotation leaves the compliance authority alone, so it is moved off the
    // old admin first to isolate the admin-only pause path.
    let compliance = env.funded_keypair();
    env.update(None, Some(compliance.pubkey()), None)
        .expect("compliance rotation must succeed");
    env.update(None, None, Some(new.pubkey()))
        .expect("nomination must succeed");
    env.accept_admin(&new).expect("acceptance must succeed");

    expect_error(
        env.grant_as(&old, &minter.pubkey(), ALLOWANCE),
        StablecoinError::Unauthorized,
    );
    expect_error(
        env.update_as(&old, Some(1_000), None, None),
        StablecoinError::Unauthorized,
    );
    expect_error(env.pause_as(&old, true), StablecoinError::Unauthorized);

    env.grant_as(&new, &minter.pubkey(), ALLOWANCE)
        .expect("the new admin must be able to grant");
    expect_error(
        env.revoke_as(&old, &minter.pubkey()),
        StablecoinError::Unauthorized,
    );
    env.revoke_as(&new, &minter.pubkey())
        .expect("the new admin must be able to revoke");
    env.update_as(&new, Some(1_000), None, None)
        .expect("the new admin must be able to update config");
    env.pause_as(&new, true)
        .expect("the new admin must be able to pause");
    env.assert_pause_states_agree(true);
}

#[test]
fn admin_rotation_leaves_compliance_authority_unchanged() {
    let mut env = Env::new();
    let old = env.admin.insecure_clone();
    let (_owner, token_account) = env.funded_account(10_000);
    let new = env.funded_keypair();

    env.update(None, None, Some(new.pubkey()))
        .expect("nomination must succeed");
    env.accept_admin(&new).expect("acceptance must succeed");

    assert_eq!(env.config_state().compliance_authority, old.pubkey());
    env.set_policy_as(&old, &token_account, PolicyStatus::Blocked)
        .expect("the original compliance authority must retain policy access");
    expect_error(
        env.set_policy_as(&new, &token_account, PolicyStatus::Allowed),
        StablecoinError::Unauthorized,
    );
}

#[test]
fn invalid_nominations_are_rejected() {
    let mut env = Env::new();
    let admin = env.admin.pubkey();

    expect_error(
        env.update(None, None, Some(Pubkey::default())),
        StablecoinError::InvalidPendingAdmin,
    );
    expect_error(
        env.update(None, None, Some(admin)),
        StablecoinError::InvalidPendingAdmin,
    );
    assert_eq!(env.config_state().pending_admin, None);
}

#[test]
fn a_newer_nomination_replaces_an_unaccepted_one() {
    let mut env = Env::new();
    let first = env.funded_keypair();
    let second = env.funded_keypair();

    env.update(None, None, Some(first.pubkey()))
        .expect("first nomination must succeed");
    env.update(None, None, Some(second.pubkey()))
        .expect("second nomination must succeed");
    assert_eq!(env.config_state().pending_admin, Some(second.pubkey()));

    expect_error(env.accept_admin(&first), StablecoinError::NoPendingAdmin);
    env.accept_admin(&second)
        .expect("the latest nominee must be able to accept");
    assert_eq!(env.config_state().admin, second.pubkey());
}

#[test]
fn accept_admin_rejects_a_substituted_config() {
    let mut env = Env::new();
    let alt_mint = env.init_alt_stablecoin();
    let nominee = env.funded_keypair();
    env.update(None, None, Some(nominee.pubkey()))
        .expect("nomination must succeed");

    let instruction = accept_admin_ix(&nominee.pubkey(), &config_pda(&alt_mint));
    expect_error(
        env.send(&[instruction], &[&nominee]),
        StablecoinError::NoPendingAdmin,
    );

    let not_a_config = accept_admin_ix(&nominee.pubkey(), &env.mint);
    expect_anchor_error(
        env.send(&[not_a_config], &[&nominee]),
        AnchorError::AccountOwnedByWrongProgram,
    );
}

#[test]
fn pause_config_and_admin_changes_emit_typed_events() {
    let mut env = Env::new();

    env.pause(true).expect("pause failed");
    let paused: Vec<PauseChanged> = env.events();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].mint, env.mint);
    assert_eq!(paused[0].authority, env.admin.pubkey());
    assert!(paused[0].paused);

    let compliance = Keypair::new();
    let successor = env.funded_keypair();
    env.update(
        Some(SUPPLY_CAP / 2),
        Some(compliance.pubkey()),
        Some(successor.pubkey()),
    )
    .expect("update failed");
    let updated: Vec<ConfigUpdated> = env.events();
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].mint, env.mint);
    assert_eq!(updated[0].admin, env.admin.pubkey());
    assert_eq!(updated[0].supply_cap, SUPPLY_CAP / 2);
    assert_eq!(updated[0].compliance_authority, compliance.pubkey());
    assert_eq!(updated[0].pending_admin, Some(successor.pubkey()));

    let previous_admin = env.admin.pubkey();
    env.accept_admin(&successor).expect("accept failed");
    let accepted: Vec<AdminAccepted> = env.events();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].mint, env.mint);
    assert_eq!(accepted[0].previous_admin, previous_admin);
    assert_eq!(accepted[0].admin, successor.pubkey());
}

#[test]
fn a_rejected_pause_emits_no_event_and_changes_nothing() {
    let mut env = Env::new();
    let intruder = env.funded_keypair();

    let before = env.config_state();
    expect_error(env.pause_as(&intruder, true), StablecoinError::Unauthorized);

    let events: Vec<PauseChanged> = env.events();
    assert!(events.is_empty());
    assert_eq!(env.config_state().paused, before.paused);
    assert!(!bool::from(env.pausable(&env.mint).paused));
}
