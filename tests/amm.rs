use amm::{
    constants::{MAX_FEE_BPS, MINIMUM_LIQUIDITY},
    errors::AmmError,
    state::{Pool, SwapDirection},
};
use anchor_lang::{
    error::ErrorCode as AnchorError, prelude::Pubkey, solana_program::system_instruction,
    AccountDeserialize, AccountSerialize, Discriminator, InstructionData, Space, ToAccountMetas,
};
use anchor_spl::token_2022::{
    spl_token_2022::{
        error::TokenError,
        extension::{
            transfer_fee::instruction::initialize_transfer_fee_config, BaseStateWithExtensions,
            ExtensionType, StateWithExtensions,
        },
        instruction::{burn_checked, initialize_account3, initialize_mint2, transfer_checked},
        state::{Account as TokenAccountState, AccountState, Mint as MintState},
    },
    ID as TOKEN_2022_ID,
};
use litesvm::{types::FailedTransactionMetadata, LiteSVM};
use solana_instruction::{AccountMeta, Instruction};
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use stablecoin::{
    constants::{CONFIG_SEED, MINTER_SEED, MINT_SEED, POLICY_SEED},
    state::{MintConfig, PolicyStatus},
};

const STABLECOIN_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/stablecoin.so"
));
const AMM_SO: &[u8] = include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/amm.so"));

const LEGACY_TOKEN_ID: Pubkey = anchor_spl::token::ID;
const DECIMALS: u8 = 6;
const SUPPLY_CAP: u64 = 1_000_000_000_000_000;
const ALLOWANCE: u64 = 100_000_000_000_000;
const FEE_BPS: u16 = 30;
const DEPOSIT_A: u64 = 1_000_000;
const DEPOSIT_B: u64 = 4_000_000;

type TxResult = Result<(), Box<FailedTransactionMetadata>>;

fn mint_pda(symbol: &[u8; 8]) -> Pubkey {
    Pubkey::find_program_address(&[MINT_SEED, symbol], &stablecoin::id()).0
}

fn config_pda(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CONFIG_SEED, mint.as_ref()], &stablecoin::id()).0
}

fn policy_pda(mint: &Pubkey, token_account: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[POLICY_SEED, mint.as_ref(), token_account.as_ref()],
        &stablecoin::id(),
    )
    .0
}

fn minter_pda(config: &Pubkey, authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[MINTER_SEED, config.as_ref(), authority.as_ref()],
        &stablecoin::id(),
    )
    .0
}

fn pool_pda(mint_a: &Pubkey, mint_b: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[amm::constants::POOL_SEED, mint_a.as_ref(), mint_b.as_ref()],
        &amm::id(),
    )
    .0
}

fn vault_pda(pool: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[amm::constants::VAULT_SEED, pool.as_ref(), mint.as_ref()],
        &amm::id(),
    )
    .0
}

fn lp_mint_pda(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[amm::constants::LP_MINT_SEED, pool.as_ref()], &amm::id()).0
}

fn locked_lp_pda(pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[amm::constants::LOCKED_LP_SEED, pool.as_ref()], &amm::id()).0
}

/// Independent restatement of the exact-in constant-product formula.
fn expected_swap_out(amount_in: u64, reserve_in: u64, reserve_out: u64, fee_bps: u16) -> u64 {
    let in_with_fee = u128::from(amount_in) * u128::from(10_000 - fee_bps);
    let numerator = in_with_fee * u128::from(reserve_out);
    let denominator = u128::from(reserve_in) * 10_000 + in_with_fee;
    u64::try_from(numerator / denominator).expect("swap output fits in u64")
}

fn floor_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut guess = value;
    let mut next = guess.div_ceil(2);
    while next < guess {
        guess = next;
        next = (guess + value / guess) / 2;
    }
    guess
}

struct User {
    keypair: Keypair,
    token_a: Pubkey,
    token_b: Pubkey,
    lp: Pubkey,
}

struct Env {
    svm: LiteSVM,
    admin: Keypair,
    mint_a: Pubkey,
    mint_b: Pubkey,
    pool: Pubkey,
    vault_a: Pubkey,
    vault_b: Pubkey,
    lp_mint: Pubkey,
    locked_lp: Pubkey,
}

impl Env {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(stablecoin::id(), STABLECOIN_SO)
            .expect("failed to load stablecoin.so; run `anchor build` first");
        svm.add_program(amm::id(), AMM_SO)
            .expect("failed to load amm.so; run `anchor build` first");

        let admin = Keypair::new();
        svm.airdrop(&admin.pubkey(), 1_000_000_000_000)
            .expect("airdrop to admin failed");

        let usdx = mint_pda(b"USDx\0\0\0\0");
        let eurx = mint_pda(b"EURx\0\0\0\0");
        let (mint_a, mint_b) = if usdx.to_bytes() < eurx.to_bytes() {
            (usdx, eurx)
        } else {
            (eurx, usdx)
        };
        let pool = pool_pda(&mint_a, &mint_b);

        let mut env = Self {
            svm,
            admin,
            mint_a,
            mint_b,
            pool,
            vault_a: vault_pda(&pool, &mint_a),
            vault_b: vault_pda(&pool, &mint_b),
            lp_mint: lp_mint_pda(&pool),
            locked_lp: locked_lp_pda(&pool),
        };

        for symbol in [b"USDx\0\0\0\0", b"EURx\0\0\0\0"] {
            env.init_stablecoin(symbol);
        }
        env
    }

    fn with_pool(fee_bps: u16) -> Self {
        let mut env = Self::new();
        env.init_pool(fee_bps).expect("pool initialization failed");
        env.allow_vaults();
        env
    }

    fn seeded(fee_bps: u16) -> (Self, User) {
        let mut env = Self::with_pool(fee_bps);
        let user = env.create_user(100_000_000, 100_000_000);
        env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0)
            .expect("first deposit failed");
        (env, user)
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

        self.svm
            .send_transaction(transaction)
            .map(|_| ())
            .map_err(Box::new)
    }

    fn init_stablecoin(&mut self, symbol: &[u8; 8]) -> Pubkey {
        let mint = mint_pda(symbol);
        let admin = self.admin.pubkey();
        let initialize = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::InitializeStablecoin {
                symbol: *symbol,
                name: "Portfolio Stablecoin".to_string(),
                uri: "https://example.com/token.json".to_string(),
                decimals: DECIMALS,
                supply_cap: SUPPLY_CAP,
            }
            .data(),
            stablecoin::accounts::InitializeStablecoin {
                payer: admin,
                mint,
                config: config_pda(&mint),
                token_program: TOKEN_2022_ID,
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        );
        let grant = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::GrantMinter {
                allowance: ALLOWANCE,
            }
            .data(),
            stablecoin::accounts::GrantMinter {
                admin,
                config: config_pda(&mint),
                authority: admin,
                minter_role: minter_pda(&config_pda(&mint), &admin),
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[initialize, grant], &[])
            .expect("stablecoin initialization failed");
        mint
    }

    fn init_pool(&mut self, fee_bps: u16) -> TxResult {
        let (mint_a, mint_b) = (self.mint_a, self.mint_b);
        let instruction = self.init_pool_ix(mint_a, mint_b, fee_bps);
        self.send(&[instruction], &[])
    }

    fn init_pool_ix(&self, mint_a: Pubkey, mint_b: Pubkey, fee_bps: u16) -> Instruction {
        let pool = pool_pda(&mint_a, &mint_b);
        Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::InitializePool { fee_bps }.data(),
            amm::accounts::InitializePool {
                payer: self.admin.pubkey(),
                mint_a,
                mint_b,
                config_a: config_pda(&mint_a),
                config_b: config_pda(&mint_b),
                pool,
                vault_a: vault_pda(&pool, &mint_a),
                vault_b: vault_pda(&pool, &mint_b),
                lp_mint: lp_mint_pda(&pool),
                locked_lp: locked_lp_pda(&pool),
                token_program: TOKEN_2022_ID,
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        )
    }

    fn allow_vaults(&mut self) {
        let (mint_a, vault_a) = (self.mint_a, self.vault_a);
        let (mint_b, vault_b) = (self.mint_b, self.vault_b);
        self.set_policy(mint_a, vault_a, PolicyStatus::Allowed)
            .expect("allowing vault_a failed");
        self.set_policy(mint_b, vault_b, PolicyStatus::Allowed)
            .expect("allowing vault_b failed");
    }

    fn set_policy(
        &mut self,
        mint: Pubkey,
        token_account: Pubkey,
        status: PolicyStatus,
    ) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::SetWalletPolicy { status }.data(),
            stablecoin::accounts::SetWalletPolicy {
                compliance_authority: self.admin.pubkey(),
                mint,
                config: config_pda(&mint),
                token_account,
                wallet_policy: policy_pda(&mint, &token_account),
                token_program: TOKEN_2022_ID,
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[])
    }

    fn set_pause(&mut self, mint: Pubkey, paused: bool) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::SetPause { paused }.data(),
            stablecoin::accounts::SetPause {
                authority: self.admin.pubkey(),
                mint,
                config: config_pda(&mint),
                token_program: TOKEN_2022_ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[])
    }

    fn create_token_account(&mut self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let mint_extensions = self.mint_extensions(mint);
        let required = ExtensionType::get_required_init_account_extensions(&mint_extensions);
        let space = ExtensionType::try_calculate_account_len::<TokenAccountState>(&required)
            .expect("account length calculation failed");

        let account = Keypair::new();
        let create = system_instruction::create_account(
            &self.admin.pubkey(),
            &account.pubkey(),
            self.svm.minimum_balance_for_rent_exemption(space),
            space as u64,
            &TOKEN_2022_ID,
        );
        let initialize = initialize_account3(&TOKEN_2022_ID, &account.pubkey(), mint, owner)
            .expect("initialize_account3 build failed");
        self.send(&[create, initialize], &[&account])
            .expect("token account creation failed");
        account.pubkey()
    }

    fn mint_stablecoin(&mut self, mint: Pubkey, destination: Pubkey, amount: u64) {
        let admin = self.admin.pubkey();
        let config = config_pda(&mint);
        let instruction = Instruction::new_with_bytes(
            stablecoin::id(),
            &stablecoin::instruction::MintTo { amount }.data(),
            stablecoin::accounts::MintTo {
                minter: admin,
                mint,
                config,
                minter_role: minter_pda(&config, &admin),
                destination,
                wallet_policy: policy_pda(&mint, &destination),
                token_program: TOKEN_2022_ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[]).expect("minting failed");
    }

    fn create_user(&mut self, amount_a: u64, amount_b: u64) -> User {
        let keypair = Keypair::new();
        self.svm
            .airdrop(&keypair.pubkey(), 10_000_000_000)
            .expect("airdrop failed");
        let owner = keypair.pubkey();

        let (mint_a, mint_b, lp_mint) = (self.mint_a, self.mint_b, self.lp_mint);
        let token_a = self.create_token_account(&owner, &mint_a);
        let token_b = self.create_token_account(&owner, &mint_b);
        self.set_policy(mint_a, token_a, PolicyStatus::Allowed)
            .expect("allowing user token_a failed");
        self.set_policy(mint_b, token_b, PolicyStatus::Allowed)
            .expect("allowing user token_b failed");
        if amount_a > 0 {
            self.mint_stablecoin(mint_a, token_a, amount_a);
        }
        if amount_b > 0 {
            self.mint_stablecoin(mint_b, token_b, amount_b);
        }

        let lp = self.create_token_account(&owner, &lp_mint);
        User {
            keypair,
            token_a,
            token_b,
            lp,
        }
    }

    fn add_accounts(&self, user: &User) -> amm::accounts::AddLiquidity {
        amm::accounts::AddLiquidity {
            user: user.keypair.pubkey(),
            pool: self.pool,
            mint_a: self.mint_a,
            mint_b: self.mint_b,
            config_a: config_pda(&self.mint_a),
            config_b: config_pda(&self.mint_b),
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            lp_mint: self.lp_mint,
            locked_lp: self.locked_lp,
            user_a: user.token_a,
            user_b: user.token_b,
            user_lp: user.lp,
            policy_a: policy_pda(&self.mint_a, &user.token_a),
            policy_b: policy_pda(&self.mint_b, &user.token_b),
            token_program: TOKEN_2022_ID,
        }
    }

    fn remove_accounts(&self, user: &User) -> amm::accounts::RemoveLiquidity {
        amm::accounts::RemoveLiquidity {
            user: user.keypair.pubkey(),
            pool: self.pool,
            mint_a: self.mint_a,
            mint_b: self.mint_b,
            config_a: config_pda(&self.mint_a),
            config_b: config_pda(&self.mint_b),
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            lp_mint: self.lp_mint,
            locked_lp: self.locked_lp,
            user_a: user.token_a,
            user_b: user.token_b,
            user_lp: user.lp,
            policy_a: policy_pda(&self.mint_a, &user.token_a),
            policy_b: policy_pda(&self.mint_b, &user.token_b),
            token_program: TOKEN_2022_ID,
        }
    }

    fn swap_accounts(&self, user: &User) -> amm::accounts::Swap {
        amm::accounts::Swap {
            user: user.keypair.pubkey(),
            pool: self.pool,
            mint_a: self.mint_a,
            mint_b: self.mint_b,
            config_a: config_pda(&self.mint_a),
            config_b: config_pda(&self.mint_b),
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            lp_mint: self.lp_mint,
            user_a: user.token_a,
            user_b: user.token_b,
            policy_a: policy_pda(&self.mint_a, &user.token_a),
            policy_b: policy_pda(&self.mint_b, &user.token_b),
            token_program: TOKEN_2022_ID,
        }
    }

    fn add_liquidity(
        &mut self,
        user: &User,
        amount_a_desired: u64,
        amount_b_desired: u64,
        amount_a_min: u64,
        amount_b_min: u64,
        min_lp_out: u64,
    ) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::AddLiquidity {
                amount_a_desired,
                amount_b_desired,
                amount_a_min,
                amount_b_min,
                min_lp_out,
            }
            .data(),
            self.add_accounts(user).to_account_metas(None),
        );
        let signer = user.keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
    }

    fn remove_liquidity(
        &mut self,
        user: &User,
        lp_amount: u64,
        min_a_out: u64,
        min_b_out: u64,
    ) -> TxResult {
        let accounts = self.remove_accounts(user);
        self.remove_liquidity_with(user, accounts, lp_amount, min_a_out, min_b_out)
    }

    fn remove_liquidity_with(
        &mut self,
        user: &User,
        accounts: amm::accounts::RemoveLiquidity,
        lp_amount: u64,
        min_a_out: u64,
        min_b_out: u64,
    ) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::RemoveLiquidity {
                lp_amount,
                min_a_out,
                min_b_out,
            }
            .data(),
            accounts.to_account_metas(None),
        );
        let signer = user.keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
    }

    fn swap(
        &mut self,
        user: &User,
        direction: SwapDirection,
        amount_in: u64,
        min_amount_out: u64,
    ) -> TxResult {
        let accounts = self.swap_accounts(user);
        self.swap_with(user, accounts, direction, amount_in, min_amount_out)
    }

    fn swap_with(
        &mut self,
        user: &User,
        accounts: amm::accounts::Swap,
        direction: SwapDirection,
        amount_in: u64,
        min_amount_out: u64,
    ) -> TxResult {
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::Swap {
                direction,
                amount_in,
                min_amount_out,
            }
            .data(),
            accounts.to_account_metas(None),
        );
        let signer = user.keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
    }

    fn donate(&mut self, user: &User, mint: Pubkey, from: Pubkey, to: Pubkey, amount: u64) {
        let instruction = transfer_checked(
            &TOKEN_2022_ID,
            &from,
            &mint,
            &to,
            &user.keypair.pubkey(),
            &[],
            amount,
            DECIMALS,
        )
        .expect("transfer_checked build failed");
        let signer = user.keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
            .expect("donation failed");
    }

    fn burn_lp(&mut self, user: &User, amount: u64) {
        let instruction = burn_checked(
            &TOKEN_2022_ID,
            &user.lp,
            &self.lp_mint,
            &user.keypair.pubkey(),
            &[],
            amount,
            DECIMALS,
        )
        .expect("burn_checked build failed");
        let signer = user.keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
            .expect("direct lp burn failed");
    }

    fn pool_state(&self) -> Pool {
        let account = self.svm.get_account(&self.pool).expect("pool missing");
        Pool::try_deserialize(&mut account.data.as_slice()).expect("pool deserialize failed")
    }

    fn mint_extensions(&self, mint: &Pubkey) -> Vec<ExtensionType> {
        let account = self.svm.get_account(mint).expect("mint missing");
        StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .get_extension_types()
            .expect("extension types unavailable")
    }

    fn token_state(&self, token_account: &Pubkey) -> TokenAccountState {
        let account = self
            .svm
            .get_account(token_account)
            .expect("token account missing");
        StateWithExtensions::<TokenAccountState>::unpack(&account.data)
            .expect("token account unpack failed")
            .base
    }

    fn token_extensions(&self, token_account: &Pubkey) -> Vec<ExtensionType> {
        let account = self
            .svm
            .get_account(token_account)
            .expect("token account missing");
        StateWithExtensions::<TokenAccountState>::unpack(&account.data)
            .expect("token account unpack failed")
            .get_extension_types()
            .expect("extension types unavailable")
    }

    fn balance(&self, token_account: &Pubkey) -> u64 {
        self.token_state(token_account).amount
    }

    fn lp_supply(&self) -> u64 {
        self.mint_state(&self.lp_mint).supply
    }

    fn mint_state(&self, mint: &Pubkey) -> MintState {
        let account = self.svm.get_account(mint).expect("mint missing");
        StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .base
    }

    fn create_raw_mint(&mut self, decimals: u8, transfer_fee: bool) -> Pubkey {
        let mint = Keypair::new();
        let extensions: &[ExtensionType] = if transfer_fee {
            &[ExtensionType::TransferFeeConfig]
        } else {
            &[]
        };
        let space = ExtensionType::try_calculate_account_len::<MintState>(extensions)
            .expect("mint length calculation failed");

        let mut instructions = vec![system_instruction::create_account(
            &self.admin.pubkey(),
            &mint.pubkey(),
            self.svm.minimum_balance_for_rent_exemption(space),
            space as u64,
            &TOKEN_2022_ID,
        )];
        if transfer_fee {
            instructions.push(
                initialize_transfer_fee_config(
                    &TOKEN_2022_ID,
                    &mint.pubkey(),
                    None,
                    None,
                    10,
                    1_000,
                )
                .expect("transfer fee initialize build failed"),
            );
        }
        instructions.push(
            initialize_mint2(
                &TOKEN_2022_ID,
                &mint.pubkey(),
                &self.admin.pubkey(),
                None,
                decimals,
            )
            .expect("initialize_mint2 build failed"),
        );
        self.send(&instructions, &[&mint])
            .expect("raw mint creation failed");
        mint.pubkey()
    }

    /// Installs a stablecoin-owned `MintConfig` at the canonical PDA of a mint the real
    /// issuer would never produce, which is the only way to reach the mint validations.
    fn fabricate_config(&mut self, mint: Pubkey, decimals: u8) {
        let (config, bump) =
            Pubkey::find_program_address(&[CONFIG_SEED, mint.as_ref()], &stablecoin::id());
        let mut data = Vec::new();
        MintConfig {
            admin: self.admin.pubkey(),
            pending_admin: None,
            compliance_authority: self.admin.pubkey(),
            mint,
            symbol: *b"FAKE\0\0\0\0",
            decimals,
            supply_cap: SUPPLY_CAP,
            total_minted: 0,
            total_burned: 0,
            paused: false,
            bump,
            mint_bump: 255,
            _reserved: [0; 64],
        }
        .try_serialize(&mut data)
        .expect("config serialize failed");
        data.resize(MintConfig::DISCRIMINATOR.len() + MintConfig::INIT_SPACE, 0);

        let mut account = self.svm.get_account(&mint).expect("mint missing");
        account.lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        account.data = data;
        account.owner = stablecoin::id();
        self.svm
            .set_account(config, account)
            .expect("set_account failed");
    }

    fn fabricate_stablecoin(&mut self, decimals: u8, transfer_fee: bool) -> Pubkey {
        let mint = self.create_raw_mint(decimals, transfer_fee);
        self.fabricate_config(mint, decimals);
        mint
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

fn expect_error(result: TxResult, expected: AmmError) {
    assert_eq!(custom_code(result), u32::from(expected));
}

fn expect_anchor_error(result: TxResult, expected: AnchorError) {
    assert_eq!(custom_code(result), u32::from(expected));
}

fn expect_token_error(result: TxResult, expected: TokenError) {
    assert_eq!(custom_code(result), expected as u32);
}

fn expect_stablecoin_error(result: TxResult, expected: stablecoin::errors::StablecoinError) {
    assert_eq!(custom_code(result), u32::from(expected));
}

#[test]
fn pool_initialization_stores_canonical_state() {
    let env = Env::with_pool(FEE_BPS);
    let pool = env.pool_state();

    assert_eq!(pool.mint_a, env.mint_a);
    assert_eq!(pool.mint_b, env.mint_b);
    assert_eq!(pool.vault_a, env.vault_a);
    assert_eq!(pool.vault_b, env.vault_b);
    assert_eq!(pool.lp_mint, env.lp_mint);
    assert_eq!(pool.locked_lp, env.locked_lp);
    assert_eq!(pool.reserve_a, 0);
    assert_eq!(pool.reserve_b, 0);
    assert_eq!(env.lp_supply(), 0);
    assert_eq!(pool.fee_bps, FEE_BPS);
    assert_eq!(pool.decimals, DECIMALS);

    assert_eq!(
        pool.bump,
        Pubkey::find_program_address(
            &[
                amm::constants::POOL_SEED,
                env.mint_a.as_ref(),
                env.mint_b.as_ref()
            ],
            &amm::id(),
        )
        .1
    );
    assert_eq!(
        pool.vault_a_bump,
        Pubkey::find_program_address(
            &[
                amm::constants::VAULT_SEED,
                env.pool.as_ref(),
                env.mint_a.as_ref()
            ],
            &amm::id(),
        )
        .1
    );
    assert_eq!(
        pool.vault_b_bump,
        Pubkey::find_program_address(
            &[
                amm::constants::VAULT_SEED,
                env.pool.as_ref(),
                env.mint_b.as_ref()
            ],
            &amm::id(),
        )
        .1
    );
    assert_eq!(
        pool.lp_mint_bump,
        Pubkey::find_program_address(
            &[amm::constants::LP_MINT_SEED, env.pool.as_ref()],
            &amm::id()
        )
        .1
    );
    assert_eq!(
        pool.locked_lp_bump,
        Pubkey::find_program_address(
            &[amm::constants::LOCKED_LP_SEED, env.pool.as_ref()],
            &amm::id()
        )
        .1
    );

    let vault_a = env.token_state(&env.vault_a);
    assert_eq!(vault_a.mint, env.mint_a);
    assert_eq!(vault_a.owner, env.pool);
    assert_eq!(vault_a.amount, 0);
}

#[test]
fn vaults_start_frozen_and_carry_the_pausable_account_extension() {
    let mut env = Env::new();
    env.init_pool(FEE_BPS).expect("pool initialization failed");

    for vault in [env.vault_a, env.vault_b] {
        assert_eq!(env.token_state(&vault).state, AccountState::Frozen);
        assert!(env
            .token_extensions(&vault)
            .contains(&ExtensionType::PausableAccount));
    }
}

#[test]
fn allowlisting_vaults_thaws_them() {
    let mut env = Env::new();
    env.init_pool(FEE_BPS).expect("pool initialization failed");
    env.allow_vaults();

    for vault in [env.vault_a, env.vault_b] {
        assert_eq!(env.token_state(&vault).state, AccountState::Initialized);
    }
}

#[test]
fn lp_mint_and_locked_account_use_the_pool_authority() {
    let env = Env::with_pool(FEE_BPS);

    let lp_mint = env.mint_state(&env.lp_mint);
    assert_eq!(lp_mint.decimals, DECIMALS);
    assert_eq!(lp_mint.supply, 0);
    assert_eq!(lp_mint.mint_authority, Some(env.pool).into());
    assert_eq!(lp_mint.freeze_authority, None.into());
    assert!(env.mint_extensions(&env.lp_mint).is_empty());

    let locked = env.token_state(&env.locked_lp);
    assert_eq!(locked.mint, env.lp_mint);
    assert_eq!(locked.owner, env.pool);
    assert_eq!(locked.amount, 0);
}

#[test]
fn zero_fee_pool_is_accepted() {
    let env = Env::with_pool(0);
    assert_eq!(env.pool_state().fee_bps, 0);
}

#[test]
fn fee_above_the_maximum_is_rejected() {
    let mut env = Env::new();
    expect_error(env.init_pool(MAX_FEE_BPS + 1), AmmError::FeeTooHigh);
}

#[test]
fn identical_and_reversed_mints_are_rejected() {
    let mut env = Env::new();
    let (mint_a, mint_b) = (env.mint_a, env.mint_b);

    let identical = env.init_pool_ix(mint_a, mint_a, FEE_BPS);
    expect_error(env.send(&[identical], &[]), AmmError::IdenticalMints);

    let reversed = env.init_pool_ix(mint_b, mint_a, FEE_BPS);
    expect_error(env.send(&[reversed], &[]), AmmError::InvalidMintOrder);
}

#[test]
fn duplicate_pool_is_rejected() {
    let mut env = Env::new();
    env.init_pool(FEE_BPS).expect("first initialization failed");

    let failure = env.init_pool(FEE_BPS).expect_err("duplicate must fail");
    assert_eq!(
        failure.err,
        TransactionError::InstructionError(0, InstructionError::Custom(0))
    );
}

#[test]
fn mint_without_a_stablecoin_config_is_rejected() {
    let mut env = Env::new();
    let plain = env.create_raw_mint(DECIMALS, false);
    let real = env.mint_a;
    let (first, second) = if plain.to_bytes() < real.to_bytes() {
        (plain, real)
    } else {
        (real, plain)
    };

    let instruction = env.init_pool_ix(first, second, FEE_BPS);
    expect_anchor_error(
        env.send(&[instruction], &[]),
        AnchorError::AccountNotInitialized,
    );
}

#[test]
fn differing_decimals_are_rejected() {
    let mut env = Env::new();
    let six = env.fabricate_stablecoin(DECIMALS, false);
    let nine = env.fabricate_stablecoin(9, false);
    let (first, second) = if six.to_bytes() < nine.to_bytes() {
        (six, nine)
    } else {
        (nine, six)
    };

    let instruction = env.init_pool_ix(first, second, FEE_BPS);
    expect_error(env.send(&[instruction], &[]), AmmError::DecimalsMismatch);
}

#[test]
fn transfer_fee_mints_are_rejected() {
    let mut env = Env::new();
    let plain = env.fabricate_stablecoin(DECIMALS, false);
    let fee_mint = env.fabricate_stablecoin(DECIMALS, true);
    let (first, second) = if plain.to_bytes() < fee_mint.to_bytes() {
        (plain, fee_mint)
    } else {
        (fee_mint, plain)
    };

    let instruction = env.init_pool_ix(first, second, FEE_BPS);
    expect_error(
        env.send(&[instruction], &[]),
        AmmError::UnsupportedMintExtension,
    );
}

#[test]
fn first_deposit_mints_expected_lp_and_locks_the_minimum() {
    let mut env = Env::with_pool(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);

    let expected_total = u64::try_from(floor_sqrt(u128::from(DEPOSIT_A) * u128::from(DEPOSIT_B)))
        .expect("lp total fits in u64");
    let expected_user = expected_total - MINIMUM_LIQUIDITY;

    env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0)
        .expect("first deposit failed");

    let pool = env.pool_state();
    assert_eq!(pool.reserve_a, DEPOSIT_A);
    assert_eq!(pool.reserve_b, DEPOSIT_B);
    assert_eq!(env.lp_supply(), expected_total);
    assert_eq!(env.balance(&user.lp), expected_user);
    assert_eq!(env.balance(&env.locked_lp), MINIMUM_LIQUIDITY);
    assert_eq!(env.balance(&env.vault_a), DEPOSIT_A);
    assert_eq!(env.balance(&env.vault_b), DEPOSIT_B);
}

#[test]
fn balanced_subsequent_deposit_mints_proportional_lp() {
    let (mut env, first) = Env::seeded(FEE_BPS);
    let second = env.create_user(100_000_000, 100_000_000);
    let before = env.pool_state();
    let before_supply = env.lp_supply();

    env.add_liquidity(&second, DEPOSIT_A / 2, DEPOSIT_B / 2, 0, 0, 0)
        .expect("balanced deposit failed");

    let expected_lp = DEPOSIT_A / 2 * before_supply / before.reserve_a;
    let pool = env.pool_state();
    assert_eq!(env.balance(&second.lp), expected_lp);
    assert_eq!(pool.reserve_a, before.reserve_a + DEPOSIT_A / 2);
    assert_eq!(pool.reserve_b, before.reserve_b + DEPOSIT_B / 2);
    assert_eq!(env.lp_supply(), before_supply + expected_lp);
    assert_eq!(env.balance(&first.lp), before_supply - MINIMUM_LIQUIDITY);
}

#[test]
fn unbalanced_deposit_pulls_only_the_quoted_amounts() {
    let (mut env, _first) = Env::seeded(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);
    let (before_a, before_b) = (env.balance(&user.token_a), env.balance(&user.token_b));

    // Offering half of A but only a quarter of B forces the ratio onto the B side.
    env.add_liquidity(&user, DEPOSIT_A / 2, DEPOSIT_B / 4, 0, 0, 0)
        .expect("unbalanced deposit failed");

    let spent_a = before_a - env.balance(&user.token_a);
    let spent_b = before_b - env.balance(&user.token_b);
    assert_eq!(spent_b, DEPOSIT_B / 4);
    assert_eq!(spent_a, DEPOSIT_A / 4);
    assert!(spent_a < DEPOSIT_A / 2);
    assert_eq!(env.balance(&user.lp), 500_000);
}

#[test]
fn deposit_slippage_bounds_are_enforced() {
    let (mut env, _first) = Env::seeded(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);

    expect_error(
        env.add_liquidity(&user, DEPOSIT_A / 2, DEPOSIT_B / 4, DEPOSIT_A / 2, 0, 0),
        AmmError::SlippageExceeded,
    );
    expect_error(
        env.add_liquidity(&user, DEPOSIT_A / 2, DEPOSIT_B / 4, 0, DEPOSIT_B, 0),
        AmmError::SlippageExceeded,
    );
    expect_error(
        env.add_liquidity(&user, DEPOSIT_A / 2, DEPOSIT_B / 4, 0, 0, 500_001),
        AmmError::SlippageExceeded,
    );

    env.add_liquidity(
        &user,
        DEPOSIT_A / 2,
        DEPOSIT_B / 4,
        DEPOSIT_A / 4,
        DEPOSIT_B / 4,
        500_000,
    )
    .expect("bounds at the quoted values must pass");
}

#[test]
fn first_deposit_below_the_locked_minimum_is_rejected() {
    let mut env = Env::with_pool(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);

    expect_error(
        env.add_liquidity(&user, MINIMUM_LIQUIDITY, MINIMUM_LIQUIDITY, 0, 0, 0),
        AmmError::InsufficientInitialLiquidity,
    );
    assert_eq!(env.lp_supply(), 0);
}

#[test]
fn zero_deposit_amounts_are_rejected() {
    let mut env = Env::with_pool(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);

    expect_error(
        env.add_liquidity(&user, 0, DEPOSIT_B, 0, 0, 0),
        AmmError::ZeroAmount,
    );
    expect_error(
        env.add_liquidity(&user, DEPOSIT_A, 0, 0, 0, 0),
        AmmError::ZeroAmount,
    );
}

#[test]
fn donation_to_a_fresh_vault_does_not_alter_reserves_or_block_the_first_deposit() {
    let mut env = Env::with_pool(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);

    let (mint_a, vault_a) = (env.mint_a, env.vault_a);
    env.donate(&user, mint_a, user.token_a, vault_a, 777_000);
    assert_eq!(env.balance(&env.vault_a), 777_000);
    assert_eq!(env.pool_state().reserve_a, 0);

    env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0)
        .expect("donation must not brick the first deposit");

    let pool = env.pool_state();
    assert_eq!(pool.reserve_a, DEPOSIT_A);
    assert_eq!(pool.reserve_b, DEPOSIT_B);
    assert_eq!(env.balance(&env.vault_a), DEPOSIT_A + 777_000);
}

#[test]
fn blocked_source_account_cannot_deposit() {
    let mut env = Env::with_pool(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);
    let mint_a = env.mint_a;
    env.set_policy(mint_a, user.token_a, PolicyStatus::Blocked)
        .expect("blocking failed");

    expect_error(
        env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0),
        AmmError::WalletNotAllowed,
    );
}

#[test]
fn deposits_fail_while_the_vaults_are_still_frozen() {
    let mut env = Env::new();
    env.init_pool(FEE_BPS).expect("pool initialization failed");
    let user = env.create_user(100_000_000, 100_000_000);

    expect_token_error(
        env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0),
        TokenError::AccountFrozen,
    );
}

#[test]
fn paused_stablecoin_blocks_deposits_and_removals() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let mint_b = env.mint_b;
    env.set_pause(mint_b, true).expect("pause failed");

    expect_error(
        env.add_liquidity(&user, DEPOSIT_A / 2, DEPOSIT_B / 2, 0, 0, 0),
        AmmError::ProtocolPaused,
    );
    expect_error(
        env.remove_liquidity(&user, 1_000, 0, 0),
        AmmError::ProtocolPaused,
    );

    env.set_pause(mint_b, false).expect("resume failed");
    env.remove_liquidity(&user, 1_000, 0, 0)
        .expect("removal must work once resumed");
}

#[test]
fn proportional_removal_returns_expected_amounts() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let before = env.pool_state();
    let before_supply = env.lp_supply();
    let lp_amount = 1_000_000;
    let expected_a = lp_amount * before.reserve_a / before_supply;
    let expected_b = lp_amount * before.reserve_b / before_supply;
    let (had_a, had_b) = (env.balance(&user.token_a), env.balance(&user.token_b));

    env.remove_liquidity(&user, lp_amount, expected_a, expected_b)
        .expect("removal failed");

    let pool = env.pool_state();
    assert_eq!(env.balance(&user.token_a) - had_a, expected_a);
    assert_eq!(env.balance(&user.token_b) - had_b, expected_b);
    assert_eq!(pool.reserve_a, before.reserve_a - expected_a);
    assert_eq!(pool.reserve_b, before.reserve_b - expected_b);
    assert_eq!(env.lp_supply(), before_supply - lp_amount);
    assert!(env.balance(&env.vault_a) >= pool.reserve_a);
    assert!(env.balance(&env.vault_b) >= pool.reserve_b);
    assert_eq!(env.balance(&env.locked_lp), MINIMUM_LIQUIDITY);
}

#[test]
fn full_removable_withdrawal_leaves_exactly_the_locked_minimum() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let removable = env.lp_supply() - MINIMUM_LIQUIDITY;

    env.remove_liquidity(&user, removable, 0, 0)
        .expect("full removal failed");

    let pool = env.pool_state();
    assert_eq!(env.lp_supply(), MINIMUM_LIQUIDITY);
    assert_eq!(env.balance(&env.locked_lp), MINIMUM_LIQUIDITY);
    assert_eq!(env.balance(&user.lp), 0);
    assert!(pool.reserve_a > 0 && pool.reserve_b > 0);
}

#[test]
fn withdrawal_slippage_is_enforced() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let before = env.pool_state();
    let before_supply = env.lp_supply();
    let lp_amount = 1_000_000;
    let expected_a = lp_amount * before.reserve_a / before_supply;

    expect_error(
        env.remove_liquidity(&user, lp_amount, expected_a + 1, 0),
        AmmError::SlippageExceeded,
    );
    assert_eq!(env.lp_supply(), before_supply);
}

#[test]
fn invalid_removal_amounts_are_rejected() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let lp_supply = env.lp_supply();

    expect_error(env.remove_liquidity(&user, 0, 0, 0), AmmError::ZeroAmount);
    expect_error(
        env.remove_liquidity(&user, lp_supply + 1, 0, 0),
        AmmError::InsufficientLiquidity,
    );
    expect_error(
        env.remove_liquidity(&user, lp_supply - MINIMUM_LIQUIDITY + 1, 0, 0),
        AmmError::InsufficientLiquidity,
    );
    assert_eq!(env.lp_supply(), lp_supply);
}

#[test]
fn blocked_destination_cannot_receive_a_withdrawal() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let mint_b = env.mint_b;
    env.set_policy(mint_b, user.token_b, PolicyStatus::Blocked)
        .expect("blocking failed");

    expect_error(
        env.remove_liquidity(&user, 1_000_000, 0, 0),
        AmmError::WalletNotAllowed,
    );
}

#[test]
fn swap_a_to_b_matches_an_independent_calculation() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let before = env.pool_state();
    let amount_in = 10_000;
    let expected_out = expected_swap_out(amount_in, before.reserve_a, before.reserve_b, FEE_BPS);
    let had_b = env.balance(&trader.token_b);

    env.swap(&trader, SwapDirection::AtoB, amount_in, expected_out)
        .expect("swap failed");

    let pool = env.pool_state();
    assert_eq!(env.balance(&trader.token_b) - had_b, expected_out);
    assert_eq!(pool.reserve_a, before.reserve_a + amount_in);
    assert_eq!(pool.reserve_b, before.reserve_b - expected_out);
    assert_eq!(env.balance(&env.vault_a), pool.reserve_a);
    assert_eq!(env.balance(&env.vault_b), pool.reserve_b);
}

#[test]
fn swap_b_to_a_matches_an_independent_calculation() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let before = env.pool_state();
    let amount_in = 40_000;
    let expected_out = expected_swap_out(amount_in, before.reserve_b, before.reserve_a, FEE_BPS);
    let had_a = env.balance(&trader.token_a);

    env.swap(&trader, SwapDirection::BtoA, amount_in, expected_out)
        .expect("swap failed");

    let pool = env.pool_state();
    assert_eq!(env.balance(&trader.token_a) - had_a, expected_out);
    assert_eq!(pool.reserve_b, before.reserve_b + amount_in);
    assert_eq!(pool.reserve_a, before.reserve_a - expected_out);
}

#[test]
fn positive_fee_keeps_value_in_the_pool_and_grows_k() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let before = env.pool_state();
    let old_k = u128::from(before.reserve_a) * u128::from(before.reserve_b);

    let amount_in = 10_000;
    let charged = expected_swap_out(amount_in, before.reserve_a, before.reserve_b, FEE_BPS);
    let free = expected_swap_out(amount_in, before.reserve_a, before.reserve_b, 0);
    assert!(charged < free, "the fee must reduce the output");

    env.swap(&trader, SwapDirection::AtoB, amount_in, 0)
        .expect("swap failed");

    let pool = env.pool_state();
    let new_k = u128::from(pool.reserve_a) * u128::from(pool.reserve_b);
    assert!(new_k > old_k, "a positive fee must grow k");
    assert_eq!(pool.reserve_b, before.reserve_b - charged);
}

#[test]
fn zero_fee_swap_never_decreases_k() {
    let (mut env, _lp) = Env::seeded(0);
    let trader = env.create_user(1_000_000, 1_000_000);
    let before = env.pool_state();
    let old_k = u128::from(before.reserve_a) * u128::from(before.reserve_b);

    env.swap(&trader, SwapDirection::AtoB, 10_000, 0)
        .expect("swap failed");

    let pool = env.pool_state();
    let new_k = u128::from(pool.reserve_a) * u128::from(pool.reserve_b);
    assert!(new_k >= old_k);
}

#[test]
fn slippage_one_unit_above_the_output_is_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let before = env.pool_state();
    let amount_in = 10_000;
    let expected_out = expected_swap_out(amount_in, before.reserve_a, before.reserve_b, FEE_BPS);

    expect_error(
        env.swap(&trader, SwapDirection::AtoB, amount_in, expected_out + 1),
        AmmError::SlippageExceeded,
    );
    env.swap(&trader, SwapDirection::AtoB, amount_in, expected_out)
        .expect("the exact quoted bound must pass");
}

#[test]
fn empty_pool_and_zero_input_swaps_are_rejected() {
    let mut env = Env::with_pool(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);

    expect_error(
        env.swap(&trader, SwapDirection::AtoB, 1_000, 0),
        AmmError::InsufficientLiquidity,
    );
    expect_error(
        env.swap(&trader, SwapDirection::AtoB, 0, 0),
        AmmError::ZeroAmount,
    );
}

#[test]
fn dust_swaps_that_round_to_zero_output_are_rejected() {
    let mut env = Env::with_pool(FEE_BPS);
    let user = env.create_user(100_000_000, 100_000_000);
    env.add_liquidity(&user, 4_000_000, 1_000, 0, 0, 0)
        .expect("lopsided deposit failed");

    expect_error(
        env.swap(&user, SwapDirection::AtoB, 1, 0),
        AmmError::ZeroOutput,
    );
}

#[test]
fn blocked_swap_accounts_are_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let (mint_a, mint_b) = (env.mint_a, env.mint_b);

    env.set_policy(mint_a, trader.token_a, PolicyStatus::Blocked)
        .expect("blocking the source failed");
    expect_error(
        env.swap(&trader, SwapDirection::AtoB, 10_000, 0),
        AmmError::WalletNotAllowed,
    );

    env.set_policy(mint_a, trader.token_a, PolicyStatus::Allowed)
        .expect("unblocking failed");
    env.set_policy(mint_b, trader.token_b, PolicyStatus::Blocked)
        .expect("blocking the destination failed");
    expect_error(
        env.swap(&trader, SwapDirection::AtoB, 10_000, 0),
        AmmError::WalletNotAllowed,
    );
}

#[test]
fn paused_stablecoin_blocks_swaps() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let mint_a = env.mint_a;
    env.set_pause(mint_a, true).expect("pause failed");

    expect_error(
        env.swap(&trader, SwapDirection::AtoB, 10_000, 0),
        AmmError::ProtocolPaused,
    );
}

#[test]
fn swapping_with_someone_elses_token_account_is_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let stranger = env.create_user(1_000_000, 1_000_000);

    let mut accounts = env.swap_accounts(&trader);
    accounts.user_a = stranger.token_a;
    accounts.policy_a = policy_pda(&env.mint_a, &stranger.token_a);
    expect_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AmmError::InvalidTokenOwner,
    );
}

#[test]
fn vault_substitutions_are_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);

    // A token account with the right mint and the pool as authority, at a non-canonical address.
    let (pool, mint_a) = (env.pool, env.mint_a);
    let lookalike = env.create_token_account(&pool, &mint_a);
    let mut accounts = env.swap_accounts(&trader);
    accounts.vault_a = lookalike;
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::ConstraintHasOne,
    );

    // A genuine vault belonging to a different pool.
    let foreign_mint = env.init_stablecoin(b"GBPx\0\0\0\0");
    let (first, second) = if foreign_mint.to_bytes() < mint_a.to_bytes() {
        (foreign_mint, mint_a)
    } else {
        (mint_a, foreign_mint)
    };
    let foreign_ix = env.init_pool_ix(first, second, FEE_BPS);
    env.send(&[foreign_ix], &[])
        .expect("foreign pool initialization failed");
    let foreign_pool = pool_pda(&first, &second);

    let mut accounts = env.swap_accounts(&trader);
    accounts.vault_a = vault_pda(&foreign_pool, &mint_a);
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::ConstraintHasOne,
    );
}

#[test]
fn wrong_lp_mint_is_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);

    let mut accounts = env.swap_accounts(&trader);
    accounts.lp_mint = env.mint_a;
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::ConstraintHasOne,
    );
}

#[test]
fn config_and_policy_substitutions_are_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let stranger = env.create_user(1_000_000, 1_000_000);

    let mut accounts = env.swap_accounts(&trader);
    accounts.config_a = config_pda(&env.mint_b);
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::ConstraintSeeds,
    );

    let mut accounts = env.swap_accounts(&trader);
    accounts.policy_a = policy_pda(&env.mint_a, &stranger.token_a);
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::ConstraintSeeds,
    );

    // A minter role has a stablecoin owner and a valid discriminator of the wrong type.
    let mut accounts = env.swap_accounts(&trader);
    accounts.config_a = minter_pda(&config_pda(&env.mint_a), &env.admin.pubkey());
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::AccountDiscriminatorMismatch,
    );
}

#[test]
fn legacy_spl_token_program_is_rejected() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);

    let mut accounts = env.swap_accounts(&trader);
    accounts.token_program = LEGACY_TOKEN_ID;
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::InvalidProgramId,
    );
}

#[test]
fn duplicate_user_and_vault_accounts_are_rejected() {
    let (mut env, lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);

    // Anchor rejects a mutable account supplied twice before any program constraint runs.
    let mut accounts = env.swap_accounts(&trader);
    accounts.user_a = env.vault_a;
    accounts.policy_a = policy_pda(&env.mint_a, &env.vault_a);
    expect_anchor_error(
        env.swap_with(&trader, accounts, SwapDirection::AtoB, 10_000, 0),
        AnchorError::ConstraintDuplicateMutableAccount,
    );

    let mut accounts = env.remove_accounts(&lp);
    accounts.user_lp = env.locked_lp;
    expect_error(
        env.remove_liquidity_with(&lp, accounts, 1_000, 0, 0),
        AmmError::DuplicateAccount,
    );
}

#[test]
fn stored_reserves_ignore_donated_vault_surplus() {
    let (mut env, lp) = Env::seeded(FEE_BPS);
    let trader = env.create_user(1_000_000, 1_000_000);
    let before = env.pool_state();

    let (mint_a, vault_a) = (env.mint_a, env.vault_a);
    env.donate(&lp, mint_a, lp.token_a, vault_a, 500_000);
    assert_eq!(env.pool_state().reserve_a, before.reserve_a);
    assert_eq!(env.balance(&env.vault_a), before.reserve_a + 500_000);

    let amount_in = 10_000;
    let expected_out = expected_swap_out(amount_in, before.reserve_a, before.reserve_b, FEE_BPS);
    let had_b = env.balance(&trader.token_b);

    env.swap(&trader, SwapDirection::AtoB, amount_in, expected_out)
        .expect("swap failed");

    assert_eq!(env.balance(&trader.token_b) - had_b, expected_out);
    assert_eq!(env.pool_state().reserve_a, before.reserve_a + amount_in);
    assert_eq!(
        env.balance(&env.vault_a),
        before.reserve_a + amount_in + 500_000
    );
}

#[test]
fn stablecoin_minting_still_requires_its_own_authority() {
    let (mut env, _lp) = Env::seeded(FEE_BPS);
    let stranger = env.create_user(0, 0);

    let instruction = Instruction::new_with_bytes(
        stablecoin::id(),
        &stablecoin::instruction::MintTo { amount: 1 }.data(),
        vec![
            AccountMeta::new_readonly(stranger.keypair.pubkey(), true),
            AccountMeta::new(env.mint_a, false),
            AccountMeta::new(config_pda(&env.mint_a), false),
            AccountMeta::new(
                minter_pda(&config_pda(&env.mint_a), &stranger.keypair.pubkey()),
                false,
            ),
            AccountMeta::new(stranger.token_a, false),
            AccountMeta::new_readonly(policy_pda(&env.mint_a, &stranger.token_a), false),
            AccountMeta::new_readonly(TOKEN_2022_ID, false),
        ],
    );
    let signer = stranger.keypair.insecure_clone();
    expect_anchor_error(
        env.send(&[instruction], &[&signer]),
        AnchorError::AccountNotInitialized,
    );
}

#[test]
fn paused_stablecoin_still_rejects_direct_transfers_into_a_vault() {
    let (mut env, lp) = Env::seeded(FEE_BPS);
    let mint_a = env.mint_a;
    env.set_pause(mint_a, true).expect("pause failed");

    let instruction = transfer_checked(
        &TOKEN_2022_ID,
        &lp.token_a,
        &mint_a,
        &env.vault_a,
        &lp.keypair.pubkey(),
        &[],
        1_000,
        DECIMALS,
    )
    .expect("transfer_checked build failed");
    let signer = lp.keypair.insecure_clone();
    expect_token_error(env.send(&[instruction], &[&signer]), TokenError::MintPaused);
}

#[test]
fn stablecoin_pause_state_is_visible_to_the_amm() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let mint_a = env.mint_a;

    env.set_pause(mint_a, true).expect("pause failed");
    expect_stablecoin_error(
        env.set_pause(mint_a, true),
        stablecoin::errors::StablecoinError::PauseStateUnchanged,
    );
    expect_error(
        env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0),
        AmmError::ProtocolPaused,
    );

    env.set_pause(mint_a, false).expect("resume failed");
    env.add_liquidity(&user, DEPOSIT_A / 2, DEPOSIT_B / 2, 0, 0, 0)
        .expect("deposit must work once resumed");
}

#[test]
fn external_lp_burn_reduces_supply_without_blocking_the_pool() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let supply_before = env.lp_supply();
    let burned = env.balance(&user.lp) / 2;

    env.burn_lp(&user, burned);
    assert_eq!(env.lp_supply(), supply_before - burned);

    let trader = env.create_user(1_000_000, 0);
    let reserves = env.pool_state();
    let amount_in = 100_000;
    let expected_out = expected_swap_out(
        amount_in,
        reserves.reserve_a,
        reserves.reserve_b,
        reserves.fee_bps,
    );
    env.swap(&trader, SwapDirection::AtoB, amount_in, expected_out)
        .expect("swap must survive an external lp burn");

    let depositor = env.create_user(100_000_000, 100_000_000);
    let pool = env.pool_state();
    let supply = env.lp_supply();
    let amount_a = DEPOSIT_A / 2;
    let amount_b =
        (u128::from(amount_a) * u128::from(pool.reserve_b)).div_ceil(u128::from(pool.reserve_a));
    let lp_from_a = u128::from(amount_a) * u128::from(supply) / u128::from(pool.reserve_a);
    let lp_from_b = amount_b * u128::from(supply) / u128::from(pool.reserve_b);
    let expected_lp = u64::try_from(lp_from_a.min(lp_from_b)).expect("lp fits in u64");

    env.add_liquidity(&depositor, amount_a, 100_000_000, 0, 0, 0)
        .expect("deposit after an external burn failed");
    assert_eq!(env.balance(&depositor.lp), expected_lp);
    assert_eq!(env.lp_supply(), supply + expected_lp);

    let remaining = env.balance(&user.lp);
    env.remove_liquidity(&user, remaining, 0, 0)
        .expect("withdrawal after an external burn failed");
    assert_eq!(env.balance(&user.lp), 0);

    let pool = env.pool_state();
    assert!(pool.reserve_a > 0 && pool.reserve_b > 0);
    assert!(env.balance(&env.vault_a) >= pool.reserve_a);
    assert!(env.balance(&env.vault_b) >= pool.reserve_b);
    assert!(env.lp_supply() >= env.balance(&env.locked_lp));
}

#[test]
fn donated_locked_lp_does_not_block_the_pool() {
    let (mut env, user) = Env::seeded(FEE_BPS);
    let (lp_mint, locked_lp) = (env.lp_mint, env.locked_lp);
    let donation = 1_000;
    env.donate(&user, lp_mint, user.lp, locked_lp, donation);

    let locked = env.balance(&env.locked_lp);
    assert_eq!(locked, MINIMUM_LIQUIDITY + donation);
    assert!(locked > MINIMUM_LIQUIDITY);

    let trader = env.create_user(1_000_000, 0);
    let reserves = env.pool_state();
    let amount_in = 100_000;
    let expected_out = expected_swap_out(
        amount_in,
        reserves.reserve_a,
        reserves.reserve_b,
        reserves.fee_bps,
    );
    env.swap(&trader, SwapDirection::AtoB, amount_in, expected_out)
        .expect("swap must survive a locked-lp donation");

    let depositor = env.create_user(100_000_000, 100_000_000);
    env.add_liquidity(&depositor, DEPOSIT_A / 2, 100_000_000, 0, 0, 0)
        .expect("deposit must survive a locked-lp donation");
    env.remove_liquidity(&user, 1_000_000, 0, 0)
        .expect("removal must survive a locked-lp donation");

    let supply = env.lp_supply();
    let locked = env.balance(&env.locked_lp);
    expect_error(
        env.remove_liquidity(&user, supply - locked + 1, 0, 0),
        AmmError::LockedLiquidityInvariant,
    );
    assert_eq!(env.lp_supply(), supply);
    assert_eq!(env.balance(&env.locked_lp), locked);
}

#[test]
fn pre_funded_pool_hierarchy_cannot_be_squatted() {
    let mut env = Env::new();
    let payer = env.admin.pubkey();
    let children = [env.vault_a, env.vault_b, env.lp_mint, env.locked_lp];
    // The runtime refuses to leave a modified account below rent exemption, so the
    // smallest possible squat is the zero-data minimum; lp_mint is overfunded instead.
    let dust = env.svm.minimum_balance_for_rent_exemption(0);
    let excess = 10_000_000_000;
    let squats: Vec<Instruction> = std::iter::once(env.pool)
        .chain(children)
        .map(|target| {
            let lamports = if target == env.lp_mint { excess } else { dust };
            system_instruction::transfer(&payer, &target, lamports)
        })
        .collect();
    env.send(&squats, &[]).expect("pre-funding failed");

    env.init_pool(FEE_BPS)
        .expect("initialization must tolerate pre-funded pdas");
    env.allow_vaults();

    let pool_account = env.svm.get_account(&env.pool).expect("pool missing");
    assert_eq!(pool_account.owner, amm::id());
    assert_eq!(pool_account.data.len(), 8 + Pool::INIT_SPACE);
    assert!(
        pool_account.lamports
            >= env
                .svm
                .minimum_balance_for_rent_exemption(pool_account.data.len())
    );

    for child in children {
        let account = env.svm.get_account(&child).expect("child pda missing");
        assert_eq!(account.owner, TOKEN_2022_ID);
        assert!(
            account.lamports
                >= env
                    .svm
                    .minimum_balance_for_rent_exemption(account.data.len())
        );
    }
    let lp_mint_account = env.svm.get_account(&env.lp_mint).expect("lp mint missing");
    assert_eq!(lp_mint_account.lamports, excess);
    assert_eq!(env.mint_state(&env.lp_mint).decimals, DECIMALS);
    assert_eq!(env.token_state(&env.locked_lp).mint, env.lp_mint);
    assert_eq!(env.token_state(&env.vault_a).mint, env.mint_a);
    assert_eq!(env.token_state(&env.vault_b).mint, env.mint_b);

    let user = env.create_user(100_000_000, 100_000_000);
    env.add_liquidity(&user, DEPOSIT_A, DEPOSIT_B, 0, 0, 0)
        .expect("pre-funded pool must remain usable");
    assert_eq!(env.balance(&env.locked_lp), MINIMUM_LIQUIDITY);
}

#[test]
fn an_occupied_child_pda_is_rejected() {
    let mut env = Env::new();
    let mut squatter = env.svm.get_account(&env.mint_a).expect("mint_a missing");
    squatter.lamports = env
        .svm
        .minimum_balance_for_rent_exemption(squatter.data.len());
    env.svm
        .set_account(env.vault_a, squatter)
        .expect("set_account failed");

    expect_error(env.init_pool(FEE_BPS), AmmError::ChildAccountInUse);
}
