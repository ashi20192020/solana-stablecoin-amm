mod support;

use anchor_lang::{
    error::ErrorCode as AnchorError,
    prelude::{Clock, Pubkey},
    solana_program::system_instruction,
    AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
};
use anchor_spl::token_2022::{
    spl_token_2022::{
        error::TokenError,
        extension::{ExtensionType, StateWithExtensions},
        instruction::{burn_checked, initialize_account3, transfer_checked},
        state::{Account as TokenAccountState, AccountState, Mint as MintState},
    },
    ID as TOKEN_2022_ID,
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
    constants::{CONFIG_SEED, MINTER_SEED, MINT_SEED, POLICY_SEED},
    errors::StablecoinError,
    events::{MinterGranted, MinterRevoked, TokensBurned, TokensMinted, WalletPolicyChanged},
    state::{MintConfig, MinterRole, PolicyStatus, WalletPolicy},
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

fn grant_minter_ix(
    admin: &Pubkey,
    config: &Pubkey,
    authority: &Pubkey,
    minter_role: &Pubkey,
    allowance: u64,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::GrantMinter { allowance }.data(),
        stablecoin::accounts::GrantMinter {
            admin: *admin,
            config: *config,
            authority: *authority,
            minter_role: *minter_role,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn revoke_minter_ix(
    admin: &Pubkey,
    config: &Pubkey,
    authority: &Pubkey,
    minter_role: &Pubkey,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::RevokeMinter {}.data(),
        stablecoin::accounts::RevokeMinter {
            admin: *admin,
            config: *config,
            authority: *authority,
            minter_role: *minter_role,
        }
        .to_account_metas(None),
    )
}

fn set_wallet_policy_ix(
    compliance_authority: &Pubkey,
    mint: &Pubkey,
    config: &Pubkey,
    token_account: &Pubkey,
    wallet_policy: &Pubkey,
    status: PolicyStatus,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::SetWalletPolicy { status }.data(),
        stablecoin::accounts::SetWalletPolicy {
            compliance_authority: *compliance_authority,
            mint: *mint,
            config: *config,
            token_account: *token_account,
            wallet_policy: *wallet_policy,
            token_program: TOKEN_2022_ID,
            system_program: anchor_lang::system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn mint_to_ix(
    minter: &Pubkey,
    mint: &Pubkey,
    config: &Pubkey,
    minter_role: &Pubkey,
    destination: &Pubkey,
    wallet_policy: &Pubkey,
    amount: u64,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::MintTo { amount }.data(),
        stablecoin::accounts::MintTo {
            minter: *minter,
            mint: *mint,
            config: *config,
            minter_role: *minter_role,
            destination: *destination,
            wallet_policy: *wallet_policy,
            token_program: TOKEN_2022_ID,
        }
        .to_account_metas(None),
    )
}

fn burn_ix(
    owner: &Pubkey,
    mint: &Pubkey,
    config: &Pubkey,
    source: &Pubkey,
    amount: u64,
) -> Instruction {
    Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::Burn { amount }.data(),
        stablecoin::accounts::Burn {
            owner: *owner,
            mint: *mint,
            config: *config,
            source: *source,
            token_program: TOKEN_2022_ID,
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
        Self::with_supply_cap(SUPPLY_CAP)
    }

    fn with_supply_cap(supply_cap: u64) -> Self {
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
        let instruction = initialize_ix(&env.admin.pubkey(), SYMBOL, supply_cap);
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
        signers.extend_from_slice(extra_signers);
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

    fn init_alt_stablecoin(&mut self) -> (Pubkey, Pubkey) {
        let mint = mint_pda(&ALT_SYMBOL);
        let instruction = initialize_ix(&self.admin.pubkey(), ALT_SYMBOL, SUPPLY_CAP);
        self.send(&[instruction], &[])
            .expect("alternate initialization failed");
        (mint, config_pda(&mint))
    }

    fn create_token_account(&mut self, owner: &Pubkey) -> Pubkey {
        self.create_token_account_for(self.mint, owner)
    }

    fn create_token_account_for(&mut self, mint: Pubkey, owner: &Pubkey) -> Pubkey {
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

    fn set_policy(&mut self, token_account: &Pubkey, status: PolicyStatus) -> TxResult {
        let instruction = set_wallet_policy_ix(
            &self.admin.pubkey(),
            &self.mint,
            &self.config,
            token_account,
            &policy_pda(&self.mint, token_account),
            status,
        );
        self.send(&[instruction], &[])
    }

    fn grant(&mut self, authority: &Pubkey, allowance: u64) -> TxResult {
        let instruction = grant_minter_ix(
            &self.admin.pubkey(),
            &self.config,
            authority,
            &minter_pda(&self.config, authority),
            allowance,
        );
        self.send(&[instruction], &[])
    }

    fn revoke(&mut self, authority: &Pubkey) -> TxResult {
        let instruction = revoke_minter_ix(
            &self.admin.pubkey(),
            &self.config,
            authority,
            &minter_pda(&self.config, authority),
        );
        self.send(&[instruction], &[])
    }

    fn mint_to(&mut self, minter: &Keypair, destination: &Pubkey, amount: u64) -> TxResult {
        let instruction = mint_to_ix(
            &minter.pubkey(),
            &self.mint,
            &self.config,
            &minter_pda(&self.config, &minter.pubkey()),
            destination,
            &policy_pda(&self.mint, destination),
            amount,
        );
        self.send(&[instruction], &[minter])
    }

    fn burn(&mut self, owner: &Keypair, source: &Pubkey, amount: u64) -> TxResult {
        let instruction = burn_ix(&owner.pubkey(), &self.mint, &self.config, source, amount);
        self.send(&[instruction], &[owner])
    }

    /// Sets up an allowed token account owned by a funded keypair.
    fn allowed_account(&mut self) -> (Keypair, Pubkey) {
        let owner = self.funded_keypair();
        let token_account = self.create_token_account(&owner.pubkey());
        self.set_policy(&token_account, PolicyStatus::Allowed)
            .expect("allowing a fresh account must succeed");
        (owner, token_account)
    }

    fn minter(&mut self, allowance: u64) -> Keypair {
        let minter = self.funded_keypair();
        self.grant(&minter.pubkey(), allowance)
            .expect("granting a minter must succeed");
        minter
    }

    /// Rewrites config state that no shipped instruction can reach yet, so the paused and
    /// counter-invariant guards can be exercised.
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
        let account = self.svm.get_account(&self.config).expect("config missing");
        MintConfig::try_deserialize(&mut account.data.as_slice())
            .expect("config deserialize failed")
    }

    fn role_state(&self, authority: &Pubkey) -> MinterRole {
        let address = minter_pda(&self.config, authority);
        let account = self.svm.get_account(&address).expect("role missing");
        MinterRole::try_deserialize(&mut account.data.as_slice()).expect("role deserialize failed")
    }

    fn policy_state(&self, token_account: &Pubkey) -> WalletPolicy {
        let address = policy_pda(&self.mint, token_account);
        let account = self.svm.get_account(&address).expect("policy missing");
        WalletPolicy::try_deserialize(&mut account.data.as_slice())
            .expect("policy deserialize failed")
    }

    fn supply(&self) -> u64 {
        let account = self.svm.get_account(&self.mint).expect("mint missing");
        StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .base
            .supply
    }

    fn balance(&self, token_account: &Pubkey) -> u64 {
        let account = self
            .svm
            .get_account(token_account)
            .expect("token account missing");
        StateWithExtensions::<TokenAccountState>::unpack(&account.data)
            .expect("token account unpack failed")
            .base
            .amount
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

    /// Direct token-2022 burns are invisible to the program counters, so tracked
    /// outstanding supply is an upper bound on the live supply rather than an equality.
    fn assert_supply_invariant(&self) {
        let config = self.config_state();
        let outstanding = config
            .total_minted
            .checked_sub(config.total_burned)
            .expect("total_burned must never exceed total_minted");
        assert!(
            outstanding >= u128::from(self.supply()),
            "total_minted - total_burned must be at least the mint supply"
        );
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
fn allowing_a_fresh_account_thaws_it() {
    let mut env = Env::new();
    let owner = env.funded_keypair();
    let token_account = env.create_token_account(&owner.pubkey());
    assert!(
        env.is_frozen(&token_account),
        "a fresh account must start frozen"
    );

    env.set_policy(&token_account, PolicyStatus::Allowed)
        .expect("allowing must succeed");

    assert!(!env.is_frozen(&token_account));
    assert_eq!(
        env.policy_state(&token_account).status,
        PolicyStatus::Allowed
    );
}

#[test]
fn blocking_an_allowed_account_freezes_it() {
    let mut env = Env::new();
    let (_owner, token_account) = env.allowed_account();

    env.set_policy(&token_account, PolicyStatus::Blocked)
        .expect("blocking must succeed");

    assert!(env.is_frozen(&token_account));
    assert_eq!(
        env.policy_state(&token_account).status,
        PolicyStatus::Blocked
    );
}

#[test]
fn allow_block_allow_cycle_keeps_policy_and_freeze_state_in_sync() {
    let mut env = Env::new();
    let owner = env.funded_keypair();
    let token_account = env.create_token_account(&owner.pubkey());

    for status in [
        PolicyStatus::Allowed,
        PolicyStatus::Blocked,
        PolicyStatus::Allowed,
    ] {
        env.set_policy(&token_account, status)
            .expect("policy change must succeed");
        assert_eq!(env.policy_state(&token_account).status, status);
        assert_eq!(
            env.is_frozen(&token_account),
            status == PolicyStatus::Blocked
        );
    }
}

#[test]
fn reapplying_the_same_status_is_idempotent() {
    let mut env = Env::new();
    let (_owner, token_account) = env.allowed_account();

    env.set_policy(&token_account, PolicyStatus::Allowed)
        .expect("reapplying allowed must succeed");
    assert!(!env.is_frozen(&token_account));

    env.set_policy(&token_account, PolicyStatus::Blocked)
        .expect("blocking must succeed");
    env.set_policy(&token_account, PolicyStatus::Blocked)
        .expect("reapplying blocked must succeed");
    assert!(env.is_frozen(&token_account));
    assert_eq!(
        env.policy_state(&token_account).status,
        PolicyStatus::Blocked
    );
}

#[test]
fn policy_records_identity_bump_and_updater() {
    let mut env = Env::new();
    let owner = env.funded_keypair();
    let token_account = env.create_token_account(&owner.pubkey());
    env.set_policy(&token_account, PolicyStatus::Allowed)
        .expect("allowing must succeed");

    let address = policy_pda(&env.mint, &token_account);
    let policy = env.policy_state(&token_account);
    assert_eq!(policy.mint, env.mint);
    assert_eq!(policy.token_account, token_account);
    assert_eq!(policy.owner, owner.pubkey());
    assert_eq!(policy.updated_by, env.admin.pubkey());
    assert_eq!(
        policy.updated_at,
        env.svm.get_sysvar::<Clock>().unix_timestamp
    );
    assert_eq!(
        Pubkey::create_program_address(
            &[
                POLICY_SEED,
                env.mint.as_ref(),
                token_account.as_ref(),
                &[policy.bump]
            ],
            &stablecoin::id()
        ),
        Ok(address)
    );
}

#[test]
fn unauthorized_policy_change_fails() {
    let mut env = Env::new();
    let owner = env.funded_keypair();
    let token_account = env.create_token_account(&owner.pubkey());
    let intruder = env.funded_keypair();

    let instruction = set_wallet_policy_ix(
        &intruder.pubkey(),
        &env.mint,
        &env.config,
        &token_account,
        &policy_pda(&env.mint, &token_account),
        PolicyStatus::Allowed,
    );
    expect_error(
        env.send(&[instruction], &[&intruder]),
        StablecoinError::Unauthorized,
    );
}

#[test]
fn policy_for_token_account_of_another_mint_fails() {
    let mut env = Env::new();
    let (alt_mint, _alt_config) = env.init_alt_stablecoin();
    let owner = env.funded_keypair();
    let foreign_account = env.create_token_account_for(alt_mint, &owner.pubkey());

    let instruction = set_wallet_policy_ix(
        &env.admin.pubkey(),
        &env.mint,
        &env.config,
        &foreign_account,
        &policy_pda(&env.mint, &foreign_account),
        PolicyStatus::Allowed,
    );
    expect_error(
        env.send(&[instruction], &[]),
        StablecoinError::TokenAccountMintMismatch,
    );
}

#[test]
fn policy_account_substitution_fails() {
    let mut env = Env::new();
    let (_owner_a, account_a) = env.allowed_account();
    let owner_b = env.funded_keypair();
    let account_b = env.create_token_account(&owner_b.pubkey());

    let instruction = set_wallet_policy_ix(
        &env.admin.pubkey(),
        &env.mint,
        &env.config,
        &account_b,
        &policy_pda(&env.mint, &account_a),
        PolicyStatus::Allowed,
    );
    expect_anchor_error(env.send(&[instruction], &[]), AnchorError::ConstraintSeeds);
}

#[test]
fn grant_and_revoke_minter() {
    let mut env = Env::new();
    let minter = env.funded_keypair();

    env.grant(&minter.pubkey(), ALLOWANCE)
        .expect("granting must succeed");
    let role = env.role_state(&minter.pubkey());
    assert_eq!(role.config, env.config);
    assert_eq!(role.authority, minter.pubkey());
    assert_eq!(role.allowance, ALLOWANCE);
    assert_eq!(role.minted, 0);
    assert_eq!(
        Pubkey::create_program_address(
            &[
                MINTER_SEED,
                env.config.as_ref(),
                minter.pubkey().as_ref(),
                &[role.bump]
            ],
            &stablecoin::id()
        ),
        Ok(minter_pda(&env.config, &minter.pubkey()))
    );

    let admin_before = env
        .svm
        .get_account(&env.admin.pubkey())
        .expect("admin missing")
        .lamports;
    env.revoke(&minter.pubkey()).expect("revoking must succeed");
    assert!(
        env.svm
            .get_account(&minter_pda(&env.config, &minter.pubkey()))
            .is_none_or(|account| account.lamports == 0),
        "the role account must be closed"
    );
    assert!(
        env.svm
            .get_account(&env.admin.pubkey())
            .expect("admin missing")
            .lamports
            > admin_before,
        "rent must be refunded to the admin"
    );
}

#[test]
fn duplicate_grant_fails() {
    let mut env = Env::new();
    let minter = env.funded_keypair();
    env.grant(&minter.pubkey(), ALLOWANCE)
        .expect("granting must succeed");

    assert_eq!(
        custom_code(env.grant(&minter.pubkey(), ALLOWANCE)),
        0,
        "the system program must reject reallocating an existing role"
    );
}

#[test]
fn unauthorized_grant_and_revoke_fail() {
    let mut env = Env::new();
    let minter = env.funded_keypair();
    env.grant(&minter.pubkey(), ALLOWANCE)
        .expect("granting must succeed");

    let intruder = env.funded_keypair();
    let role = minter_pda(&env.config, &minter.pubkey());

    let grant = grant_minter_ix(
        &intruder.pubkey(),
        &env.config,
        &intruder.pubkey(),
        &minter_pda(&env.config, &intruder.pubkey()),
        ALLOWANCE,
    );
    expect_error(
        env.send(&[grant], &[&intruder]),
        StablecoinError::Unauthorized,
    );

    let revoke = revoke_minter_ix(&intruder.pubkey(), &env.config, &minter.pubkey(), &role);
    expect_error(
        env.send(&[revoke], &[&intruder]),
        StablecoinError::Unauthorized,
    );
}

#[test]
fn zero_allowance_and_default_authority_are_rejected() {
    let mut env = Env::new();
    let minter = env.funded_keypair();

    expect_error(
        env.grant(&minter.pubkey(), 0),
        StablecoinError::ZeroAllowance,
    );
    expect_error(
        env.grant(&Pubkey::default(), ALLOWANCE),
        StablecoinError::InvalidAuthority,
    );
}

#[test]
fn mint_to_allowed_account_updates_supply_role_and_counters() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    let amount = 125_000_000;

    env.mint_to(&minter, &destination, amount)
        .expect("minting must succeed");

    assert_eq!(env.balance(&destination), amount);
    assert_eq!(env.supply(), amount);
    assert_eq!(env.role_state(&minter.pubkey()).minted, amount);
    let config = env.config_state();
    assert_eq!(config.total_minted, u128::from(amount));
    assert_eq!(config.total_burned, 0);
    env.assert_supply_invariant();
}

#[test]
fn mint_exactly_to_allowance_succeeds_and_one_more_fails() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);

    env.mint_to(&minter, &destination, ALLOWANCE)
        .expect("minting the full allowance must succeed");
    assert_eq!(env.role_state(&minter.pubkey()).minted, ALLOWANCE);
    env.assert_supply_invariant();

    expect_error(
        env.mint_to(&minter, &destination, 1),
        StablecoinError::AllowanceExceeded,
    );
}

#[test]
fn mint_exactly_to_supply_cap_succeeds_and_one_more_fails() {
    let cap = 1_000_000;
    let mut env = Env::with_supply_cap(cap);
    let (_owner, destination) = env.allowed_account();
    let minter = env.minter(u64::MAX);

    env.mint_to(&minter, &destination, cap)
        .expect("minting up to the cap must succeed");
    assert_eq!(env.supply(), cap);
    env.assert_supply_invariant();

    expect_error(
        env.mint_to(&minter, &destination, 1),
        StablecoinError::SupplyCapExceeded,
    );
}

#[test]
fn zero_mint_amount_is_rejected() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);

    expect_error(
        env.mint_to(&minter, &destination, 0),
        StablecoinError::ZeroAmount,
    );
}

#[test]
fn non_minter_and_revoked_minter_cannot_mint() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let stranger = env.funded_keypair();

    expect_anchor_error(
        env.mint_to(&stranger, &destination, 1),
        AnchorError::AccountNotInitialized,
    );

    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &destination, 1)
        .expect("granted minter must be able to mint");
    env.revoke(&minter.pubkey()).expect("revoking must succeed");

    expect_anchor_error(
        env.mint_to(&minter, &destination, 1),
        AnchorError::AccountNotInitialized,
    );
}

#[test]
fn role_from_another_config_or_authority_is_rejected() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let (_alt_mint, alt_config) = env.init_alt_stablecoin();

    let minter = env.funded_keypair();
    let foreign_role = minter_pda(&alt_config, &minter.pubkey());
    let grant = grant_minter_ix(
        &env.admin.pubkey(),
        &alt_config,
        &minter.pubkey(),
        &foreign_role,
        ALLOWANCE,
    );
    env.send(&[grant], &[])
        .expect("granting on the alternate config must succeed");

    let with_foreign_role = mint_to_ix(
        &minter.pubkey(),
        &env.mint,
        &env.config,
        &foreign_role,
        &destination,
        &policy_pda(&env.mint, &destination),
        1,
    );
    expect_anchor_error(
        env.send(&[with_foreign_role], &[&minter]),
        AnchorError::ConstraintSeeds,
    );

    let other = env.minter(ALLOWANCE);
    let with_other_role = mint_to_ix(
        &minter.pubkey(),
        &env.mint,
        &env.config,
        &minter_pda(&env.config, &other.pubkey()),
        &destination,
        &policy_pda(&env.mint, &destination),
        1,
    );
    expect_anchor_error(
        env.send(&[with_other_role], &[&minter]),
        AnchorError::ConstraintSeeds,
    );
}

#[test]
fn minting_into_missing_or_blocked_policy_accounts_fails() {
    let mut env = Env::new();
    let minter = env.minter(ALLOWANCE);

    let owner = env.funded_keypair();
    let unpoliced = env.create_token_account(&owner.pubkey());
    expect_anchor_error(
        env.mint_to(&minter, &unpoliced, 1),
        AnchorError::AccountNotInitialized,
    );

    let (_blocked_owner, blocked) = env.allowed_account();
    env.set_policy(&blocked, PolicyStatus::Blocked)
        .expect("blocking must succeed");
    expect_error(
        env.mint_to(&minter, &blocked, 1),
        StablecoinError::WalletNotAllowed,
    );
}

#[test]
fn partial_and_full_burn_update_supply_and_counters() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    let minted = 400_000_000;

    env.mint_to(&minter, &token_account, minted)
        .expect("minting must succeed");

    env.burn(&owner, &token_account, 150_000_000)
        .expect("partial burn must succeed");
    assert_eq!(env.balance(&token_account), 250_000_000);
    assert_eq!(env.supply(), 250_000_000);
    assert_eq!(env.config_state().total_burned, 150_000_000);
    env.assert_supply_invariant();

    env.burn(&owner, &token_account, 250_000_000)
        .expect("full burn must succeed");
    assert_eq!(env.balance(&token_account), 0);
    assert_eq!(env.supply(), 0);
    let config = env.config_state();
    assert_eq!(config.total_minted, u128::from(minted));
    assert_eq!(config.total_burned, u128::from(minted));
    env.assert_supply_invariant();
}

#[test]
fn zero_burn_is_rejected() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();

    expect_error(
        env.burn(&owner, &token_account, 0),
        StablecoinError::ZeroAmount,
    );
}

#[test]
fn burn_by_non_owner_is_rejected() {
    let mut env = Env::new();
    let (_owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &token_account, 1_000)
        .expect("minting must succeed");

    let stranger = env.funded_keypair();
    expect_error(
        env.burn(&stranger, &token_account, 1),
        StablecoinError::Unauthorized,
    );
}

#[test]
fn burn_above_balance_is_rejected_by_token_2022() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &token_account, 1_000)
        .expect("minting must succeed");

    expect_token_error(
        env.burn(&owner, &token_account, 1_001),
        TokenError::InsufficientFunds,
    );
}

#[test]
fn burn_from_a_blocked_account_is_rejected_by_token_2022() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &token_account, 1_000)
        .expect("minting must succeed");
    env.set_policy(&token_account, PolicyStatus::Blocked)
        .expect("blocking must succeed");

    expect_token_error(
        env.burn(&owner, &token_account, 1),
        TokenError::AccountFrozen,
    );
}

#[test]
fn direct_transfers_touching_a_blocked_account_are_rejected() {
    let mut env = Env::new();
    let (sender, source) = env.allowed_account();
    let (_receiver, destination) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &source, 10_000)
        .expect("minting must succeed");

    let transfer = |from: &Pubkey, to: &Pubkey, authority: &Pubkey, mint: &Pubkey| {
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

    env.set_policy(&source, PolicyStatus::Blocked)
        .expect("blocking the source must succeed");
    let blocked_source = transfer(&source, &destination, &sender.pubkey(), &env.mint);
    expect_token_error(
        env.send(&[blocked_source], &[&sender]),
        TokenError::AccountFrozen,
    );

    env.set_policy(&source, PolicyStatus::Allowed)
        .expect("re-allowing the source must succeed");
    env.set_policy(&destination, PolicyStatus::Blocked)
        .expect("blocking the destination must succeed");
    let blocked_destination = transfer(&source, &destination, &sender.pubkey(), &env.mint);
    expect_token_error(
        env.send(&[blocked_destination], &[&sender]),
        TokenError::AccountFrozen,
    );
}

#[test]
fn paused_protocol_blocks_issuance_but_not_policy_changes() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &token_account, 1_000)
        .expect("minting must succeed");

    env.patch_config(|config| config.paused = true);

    expect_error(
        env.mint_to(&minter, &token_account, 1),
        StablecoinError::ProtocolPaused,
    );
    expect_error(
        env.burn(&owner, &token_account, 1),
        StablecoinError::ProtocolPaused,
    );
    env.set_policy(&token_account, PolicyStatus::Blocked)
        .expect("freezing must remain available while paused");
    assert!(env.is_frozen(&token_account));
}

#[test]
fn broken_supply_counters_block_issuance() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &token_account, 1_000)
        .expect("minting must succeed");

    // Burning more than was ever issued underflows the tracked outstanding supply.
    env.patch_config(|config| config.total_burned = config.total_minted + 1);
    expect_error(
        env.mint_to(&minter, &token_account, 1),
        StablecoinError::CounterInvariantViolation,
    );
    expect_error(
        env.burn(&owner, &token_account, 1),
        StablecoinError::CounterInvariantViolation,
    );

    // Tracked outstanding supply below the live supply is equally inconsistent.
    env.patch_config(|config| {
        config.total_minted = 999;
        config.total_burned = 0;
    });
    expect_error(
        env.mint_to(&minter, &token_account, 1),
        StablecoinError::CounterInvariantViolation,
    );
    expect_error(
        env.burn(&owner, &token_account, 1),
        StablecoinError::CounterInvariantViolation,
    );
}

#[test]
fn substituted_mint_and_token_accounts_are_rejected() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    let (alt_mint, _alt_config) = env.init_alt_stablecoin();

    let foreign_mint = mint_to_ix(
        &minter.pubkey(),
        &alt_mint,
        &env.config,
        &minter_pda(&env.config, &minter.pubkey()),
        &destination,
        &policy_pda(&env.mint, &destination),
        1,
    );
    expect_anchor_error(
        env.send(&[foreign_mint], &[&minter]),
        AnchorError::ConstraintSeeds,
    );

    // A token account of another mint can never hold a policy under this mint, because the
    // policy PDA is seeded by both, so the substitution is caught before the mint check.
    let owner = env.funded_keypair();
    let foreign_account = env.create_token_account_for(alt_mint, &owner.pubkey());
    let foreign_destination = mint_to_ix(
        &minter.pubkey(),
        &env.mint,
        &env.config,
        &minter_pda(&env.config, &minter.pubkey()),
        &foreign_account,
        &policy_pda(&env.mint, &foreign_account),
        1,
    );
    expect_anchor_error(
        env.send(&[foreign_destination], &[&minter]),
        AnchorError::AccountNotInitialized,
    );

    let foreign_burn = burn_ix(&owner.pubkey(), &env.mint, &env.config, &foreign_account, 1);
    expect_error(
        env.send(&[foreign_burn], &[&owner]),
        StablecoinError::TokenAccountMintMismatch,
    );
}

#[test]
fn substituted_role_and_policy_accounts_are_rejected() {
    let mut env = Env::new();
    let (_owner, destination) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);

    let role_is_config = mint_to_ix(
        &minter.pubkey(),
        &env.mint,
        &env.config,
        &env.config,
        &destination,
        &policy_pda(&env.mint, &destination),
        1,
    );
    expect_anchor_error(
        env.send(&[role_is_config], &[&minter]),
        AnchorError::AccountDiscriminatorMismatch,
    );

    let (_other_owner, other_account) = env.allowed_account();
    let foreign_policy = mint_to_ix(
        &minter.pubkey(),
        &env.mint,
        &env.config,
        &minter_pda(&env.config, &minter.pubkey()),
        &destination,
        &policy_pda(&env.mint, &other_account),
        1,
    );
    expect_anchor_error(
        env.send(&[foreign_policy], &[&minter]),
        AnchorError::ConstraintSeeds,
    );
}

#[test]
fn minter_lifecycle_emits_typed_events() {
    let mut env = Env::new();
    let minter = env.funded_keypair();

    env.grant(&minter.pubkey(), ALLOWANCE)
        .expect("grant failed");
    let granted: Vec<MinterGranted> = env.events();
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0].config, env.config);
    assert_eq!(granted[0].authority, minter.pubkey());
    assert_eq!(granted[0].allowance, ALLOWANCE);

    let destination = env.create_token_account(&minter.pubkey());
    env.set_policy(&destination, PolicyStatus::Allowed)
        .expect("policy failed");
    let policies: Vec<WalletPolicyChanged> = env.events();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].mint, env.mint);
    assert_eq!(policies[0].token_account, destination);
    assert_eq!(policies[0].owner, minter.pubkey());
    assert_eq!(policies[0].status, PolicyStatus::Allowed);
    assert_eq!(policies[0].updated_by, env.admin.pubkey());

    env.mint_to(&minter, &destination, 5_000)
        .expect("mint failed");
    let minted: Vec<TokensMinted> = env.events();
    assert_eq!(minted.len(), 1);
    assert_eq!(minted[0].mint, env.mint);
    assert_eq!(minted[0].minter, minter.pubkey());
    assert_eq!(minted[0].destination, destination);
    assert_eq!(minted[0].amount, 5_000);
    assert_eq!(minted[0].minter_minted, 5_000);
    assert_eq!(minted[0].supply, 5_000);

    env.burn(&minter, &destination, 2_000).expect("burn failed");
    let burned: Vec<TokensBurned> = env.events();
    assert_eq!(burned.len(), 1);
    assert_eq!(burned[0].owner, minter.pubkey());
    assert_eq!(burned[0].source, destination);
    assert_eq!(burned[0].amount, 2_000);
    assert_eq!(burned[0].supply, 3_000);

    env.revoke(&minter.pubkey()).expect("revoke failed");
    let revoked: Vec<MinterRevoked> = env.events();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].authority, minter.pubkey());
    assert_eq!(revoked[0].minted, 5_000);
}

#[test]
fn a_rejected_mint_emits_no_event_and_changes_nothing() {
    let mut env = Env::new();
    let minter = env.funded_keypair();
    env.grant(&minter.pubkey(), ALLOWANCE)
        .expect("grant failed");
    let destination = env.create_token_account(&minter.pubkey());
    env.set_policy(&destination, PolicyStatus::Allowed)
        .expect("policy failed");

    let before = env.config_state();
    expect_error(
        env.mint_to(&minter, &destination, ALLOWANCE + 1),
        StablecoinError::AllowanceExceeded,
    );

    let minted: Vec<TokensMinted> = env.events();
    assert!(minted.is_empty());
    let after = env.config_state();
    assert_eq!(after.total_minted, before.total_minted);
    assert_eq!(env.supply(), 0);
    assert_eq!(env.balance(&destination), 0);
}

#[test]
fn a_direct_token_2022_burn_leaves_the_program_counters_untouched() {
    let mut env = Env::new();
    let (owner, token_account) = env.allowed_account();
    let minter = env.minter(ALLOWANCE);
    env.mint_to(&minter, &token_account, 10_000)
        .expect("minting must succeed");

    let before = env.config_state();
    let instruction = burn_checked(
        &TOKEN_2022_ID,
        &token_account,
        &env.mint,
        &owner.pubkey(),
        &[],
        4_000,
        DECIMALS,
    )
    .expect("burn_checked build failed");
    env.send(&[instruction], &[&owner])
        .expect("an owner may burn a thawed balance through token-2022");

    assert_eq!(env.supply(), 6_000);
    assert_eq!(env.balance(&token_account), 6_000);
    let after = env.config_state();
    assert_eq!(after.total_minted, before.total_minted);
    assert_eq!(after.total_burned, before.total_burned);
    assert!(env.events::<TokensBurned>().is_empty());

    // The slack the direct burn opened must not block program-mediated operations.
    env.mint_to(&minter, &token_account, 2_000)
        .expect("minting must still succeed");
    env.burn(&owner, &token_account, 1_000)
        .expect("program burning must still succeed");
    let instruction = Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::UpdateConfig {
            supply_cap: Some(SUPPLY_CAP / 2),
            compliance_authority: None,
            pending_admin: None,
        }
        .data(),
        stablecoin::accounts::UpdateConfig {
            admin: env.admin.pubkey(),
            mint: env.mint,
            config: env.config,
        }
        .to_account_metas(None),
    );
    env.send(&[instruction], &[])
        .expect("supply cap updates must still succeed");

    let final_config = env.config_state();
    let supply = env.supply();
    assert_eq!(supply, 7_000);
    assert!(supply <= final_config.supply_cap);
    let outstanding = final_config
        .total_minted
        .checked_sub(final_config.total_burned)
        .expect("tracked outstanding supply must not underflow");
    assert!(outstanding >= u128::from(supply));
    assert_eq!(final_config.total_minted, before.total_minted + 2_000);
    assert_eq!(final_config.total_burned, before.total_burned + 1_000);
}
