//! Deterministic operation sequence that re-asserts every protocol invariant after each
//! step, including the steps that are expected to be rejected.

use amm::{
    constants::MINIMUM_LIQUIDITY,
    errors::AmmError,
    state::{Pool, SwapDirection},
};
use anchor_lang::{
    prelude::Pubkey, solana_program::system_instruction, AccountDeserialize, InstructionData,
    ToAccountMetas,
};
use anchor_spl::token_2022::{
    spl_token_2022::{
        extension::{
            pausable::{PausableAccount, PausableConfig},
            BaseStateWithExtensions, ExtensionType, StateWithExtensions,
        },
        instruction::{burn_checked, initialize_account3},
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
    constants::{CONFIG_SEED, MAX_SUPPLY_CAP, MINTER_SEED, MINT_SEED, POLICY_SEED},
    state::{MintConfig, MinterRole, PolicyStatus},
};

const STABLECOIN_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_TARGET_TMPDIR"),
    "/../deploy/stablecoin.so"
));
const AMM_SO: &[u8] = include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/amm.so"));

const DECIMALS: u8 = 6;
const SUPPLY_CAP: u64 = 1_000_000_000_000_000;
const ALLOWANCE: u64 = 100_000_000_000_000;
const FEE_BPS: u16 = 30;
const ACTORS: usize = 3;
const OPERATIONS: usize = 192;
const ENDOWMENT: u64 = 400_000_000;
const SEED_A: u64 = 2_000_000;
const SEED_B: u64 = 8_000_000;

/// xorshift64*, so the sequence is reproducible without a dependency.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.state = state;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }
}

struct Actor {
    keypair: Keypair,
    token_a: Pubkey,
    token_b: Pubkey,
    lp: Pubkey,
    allowed_a: bool,
    allowed_b: bool,
}

/// Everything the assertions read back from the ledger after each operation.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    reserve_a: u64,
    reserve_b: u64,
    lp_supply: u64,
    locked_lp: u64,
    vault_a: u64,
    vault_b: u64,
    supply_a: u64,
    supply_b: u64,
    counters_a: (u128, u128),
    counters_b: (u128, u128),
    paused_a: bool,
    paused_b: bool,
    balances: Vec<(u64, u64, u64)>,
}

struct World {
    svm: LiteSVM,
    admin: Keypair,
    mint_a: Pubkey,
    mint_b: Pubkey,
    pool: Pubkey,
    vault_a: Pubkey,
    vault_b: Pubkey,
    lp_mint: Pubkey,
    locked_lp: Pubkey,
    actors: Vec<Actor>,
    paused_a: bool,
    paused_b: bool,
}

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

type TxResult = Result<(), Box<FailedTransactionMetadata>>;

impl World {
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
        let pool = Pubkey::find_program_address(
            &[amm::constants::POOL_SEED, mint_a.as_ref(), mint_b.as_ref()],
            &amm::id(),
        )
        .0;
        let vault = |mint: &Pubkey| {
            Pubkey::find_program_address(
                &[amm::constants::VAULT_SEED, pool.as_ref(), mint.as_ref()],
                &amm::id(),
            )
            .0
        };

        let mut world = Self {
            svm,
            admin,
            mint_a,
            mint_b,
            pool,
            vault_a: vault(&mint_a),
            vault_b: vault(&mint_b),
            lp_mint: Pubkey::find_program_address(
                &[amm::constants::LP_MINT_SEED, pool.as_ref()],
                &amm::id(),
            )
            .0,
            locked_lp: Pubkey::find_program_address(
                &[amm::constants::LOCKED_LP_SEED, pool.as_ref()],
                &amm::id(),
            )
            .0,
            actors: Vec::new(),
            paused_a: false,
            paused_b: false,
        };

        for symbol in [b"USDx\0\0\0\0", b"EURx\0\0\0\0"] {
            world.init_stablecoin(symbol);
        }
        world.init_pool();
        let (mint_a, vault_a, mint_b, vault_b) =
            (world.mint_a, world.vault_a, world.mint_b, world.vault_b);
        world
            .set_policy(mint_a, vault_a, PolicyStatus::Allowed)
            .expect("allowing vault_a failed");
        world
            .set_policy(mint_b, vault_b, PolicyStatus::Allowed)
            .expect("allowing vault_b failed");

        for _ in 0..ACTORS {
            let actor = world.create_actor();
            world.actors.push(actor);
        }
        world
            .add_liquidity(0, SEED_A, SEED_B)
            .expect("seed deposit failed");
        world
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

    fn init_stablecoin(&mut self, symbol: &[u8; 8]) {
        let mint = mint_pda(symbol);
        let config = config_pda(&mint);
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
                config,
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
                config,
                authority: admin,
                minter_role: minter_pda(&config, &admin),
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[initialize, grant], &[])
            .expect("stablecoin initialization failed");
    }

    fn init_pool(&mut self) {
        let (mint_a, mint_b) = (self.mint_a, self.mint_b);
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::InitializePool { fee_bps: FEE_BPS }.data(),
            amm::accounts::InitializePool {
                payer: self.admin.pubkey(),
                mint_a,
                mint_b,
                config_a: config_pda(&mint_a),
                config_b: config_pda(&mint_b),
                pool: self.pool,
                vault_a: self.vault_a,
                vault_b: self.vault_b,
                lp_mint: self.lp_mint,
                locked_lp: self.locked_lp,
                token_program: TOKEN_2022_ID,
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[instruction], &[])
            .expect("pool initialization failed");
    }

    fn create_token_account(&mut self, owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        let account = self.svm.get_account(mint).expect("mint missing");
        let extensions = StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .get_extension_types()
            .expect("extension types unavailable");
        let required = ExtensionType::get_required_init_account_extensions(&extensions);
        let space = ExtensionType::try_calculate_account_len::<TokenAccountState>(&required)
            .expect("account length calculation failed");

        let keypair = Keypair::new();
        let create = system_instruction::create_account(
            &self.admin.pubkey(),
            &keypair.pubkey(),
            self.svm.minimum_balance_for_rent_exemption(space),
            space as u64,
            &TOKEN_2022_ID,
        );
        let initialize = initialize_account3(&TOKEN_2022_ID, &keypair.pubkey(), mint, owner)
            .expect("initialize_account3 build failed");
        self.send(&[create, initialize], &[&keypair])
            .expect("token account creation failed");
        keypair.pubkey()
    }

    fn create_actor(&mut self) -> Actor {
        let keypair = Keypair::new();
        self.svm
            .airdrop(&keypair.pubkey(), 10_000_000_000)
            .expect("airdrop failed");
        let owner = keypair.pubkey();
        let (mint_a, mint_b, lp_mint) = (self.mint_a, self.mint_b, self.lp_mint);

        let token_a = self.create_token_account(&owner, &mint_a);
        let token_b = self.create_token_account(&owner, &mint_b);
        let lp = self.create_token_account(&owner, &lp_mint);
        self.set_policy(mint_a, token_a, PolicyStatus::Allowed)
            .expect("allowing token_a failed");
        self.set_policy(mint_b, token_b, PolicyStatus::Allowed)
            .expect("allowing token_b failed");
        self.mint_stablecoin(mint_a, token_a, ENDOWMENT);
        self.mint_stablecoin(mint_b, token_b, ENDOWMENT);

        Actor {
            keypair,
            token_a,
            token_b,
            lp,
            allowed_a: true,
            allowed_b: true,
        }
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

    /// Burns straight through token-2022, bypassing this program entirely.
    fn direct_burn(&mut self, actor: usize, mint: Pubkey, token_account: Pubkey, amount: u64) {
        let owner = self.actors[actor].keypair.insecure_clone();
        let instruction = burn_checked(
            &TOKEN_2022_ID,
            &token_account,
            &mint,
            &owner.pubkey(),
            &[],
            amount,
            DECIMALS,
        )
        .expect("burn_checked build failed");
        self.send(&[instruction], &[&owner])
            .expect("direct burn failed");
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

    fn add_liquidity(&mut self, actor: usize, amount_a: u64, amount_b: u64) -> TxResult {
        let accounts = amm::accounts::AddLiquidity {
            user: self.actors[actor].keypair.pubkey(),
            pool: self.pool,
            mint_a: self.mint_a,
            mint_b: self.mint_b,
            config_a: config_pda(&self.mint_a),
            config_b: config_pda(&self.mint_b),
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            lp_mint: self.lp_mint,
            locked_lp: self.locked_lp,
            user_a: self.actors[actor].token_a,
            user_b: self.actors[actor].token_b,
            user_lp: self.actors[actor].lp,
            policy_a: policy_pda(&self.mint_a, &self.actors[actor].token_a),
            policy_b: policy_pda(&self.mint_b, &self.actors[actor].token_b),
            token_program: TOKEN_2022_ID,
        };
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::AddLiquidity {
                amount_a_desired: amount_a,
                amount_b_desired: amount_b,
                amount_a_min: 0,
                amount_b_min: 0,
                min_lp_out: 0,
            }
            .data(),
            accounts.to_account_metas(None),
        );
        let signer = self.actors[actor].keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
    }

    fn remove_liquidity(&mut self, actor: usize, lp_amount: u64) -> TxResult {
        let accounts = amm::accounts::RemoveLiquidity {
            user: self.actors[actor].keypair.pubkey(),
            pool: self.pool,
            mint_a: self.mint_a,
            mint_b: self.mint_b,
            config_a: config_pda(&self.mint_a),
            config_b: config_pda(&self.mint_b),
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            lp_mint: self.lp_mint,
            locked_lp: self.locked_lp,
            user_a: self.actors[actor].token_a,
            user_b: self.actors[actor].token_b,
            user_lp: self.actors[actor].lp,
            policy_a: policy_pda(&self.mint_a, &self.actors[actor].token_a),
            policy_b: policy_pda(&self.mint_b, &self.actors[actor].token_b),
            token_program: TOKEN_2022_ID,
        };
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::RemoveLiquidity {
                lp_amount,
                min_a_out: 0,
                min_b_out: 0,
            }
            .data(),
            accounts.to_account_metas(None),
        );
        let signer = self.actors[actor].keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
    }

    fn swap(&mut self, actor: usize, direction: SwapDirection, amount_in: u64) -> TxResult {
        let accounts = amm::accounts::Swap {
            user: self.actors[actor].keypair.pubkey(),
            pool: self.pool,
            mint_a: self.mint_a,
            mint_b: self.mint_b,
            config_a: config_pda(&self.mint_a),
            config_b: config_pda(&self.mint_b),
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            lp_mint: self.lp_mint,
            user_a: self.actors[actor].token_a,
            user_b: self.actors[actor].token_b,
            policy_a: policy_pda(&self.mint_a, &self.actors[actor].token_a),
            policy_b: policy_pda(&self.mint_b, &self.actors[actor].token_b),
            token_program: TOKEN_2022_ID,
        };
        let instruction = Instruction::new_with_bytes(
            amm::id(),
            &amm::instruction::Swap {
                direction,
                amount_in,
                min_amount_out: 0,
            }
            .data(),
            accounts.to_account_metas(None),
        );
        let signer = self.actors[actor].keypair.insecure_clone();
        self.send(&[instruction], &[&signer])
    }

    fn pool_state(&self) -> Pool {
        let account = self.svm.get_account(&self.pool).expect("pool missing");
        Pool::try_deserialize(&mut account.data.as_slice()).expect("pool deserialize failed")
    }

    fn config_state(&self, mint: &Pubkey) -> MintConfig {
        let account = self
            .svm
            .get_account(&config_pda(mint))
            .expect("config missing");
        MintConfig::try_deserialize(&mut account.data.as_slice())
            .expect("config deserialize failed")
    }

    fn mint_state(&self, mint: &Pubkey) -> MintState {
        let account = self.svm.get_account(mint).expect("mint missing");
        StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .base
    }

    fn pausable(&self, mint: &Pubkey) -> PausableConfig {
        let account = self.svm.get_account(mint).expect("mint missing");
        StateWithExtensions::<MintState>::unpack(&account.data)
            .expect("mint unpack failed")
            .get_extension::<PausableConfig>()
            .copied()
            .expect("pausable extension missing")
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

    fn has_pausable_account(&self, token_account: &Pubkey) -> bool {
        let account = self
            .svm
            .get_account(token_account)
            .expect("token account missing");
        StateWithExtensions::<TokenAccountState>::unpack(&account.data)
            .expect("token account unpack failed")
            .get_extension::<PausableAccount>()
            .is_ok()
    }

    fn balance(&self, token_account: &Pubkey) -> u64 {
        self.token_state(token_account).amount
    }

    fn lp_supply(&self) -> u64 {
        self.mint_state(&self.lp_mint).supply
    }

    fn snapshot(&self) -> Snapshot {
        let pool = self.pool_state();
        let config_a = self.config_state(&self.mint_a);
        let config_b = self.config_state(&self.mint_b);
        Snapshot {
            reserve_a: pool.reserve_a,
            reserve_b: pool.reserve_b,
            lp_supply: self.lp_supply(),
            locked_lp: self.balance(&self.locked_lp),
            vault_a: self.balance(&self.vault_a),
            vault_b: self.balance(&self.vault_b),
            supply_a: self.mint_state(&self.mint_a).supply,
            supply_b: self.mint_state(&self.mint_b).supply,
            counters_a: (config_a.total_minted, config_a.total_burned),
            counters_b: (config_b.total_minted, config_b.total_burned),
            paused_a: config_a.paused,
            paused_b: config_b.paused,
            balances: self
                .actors
                .iter()
                .map(|actor| {
                    (
                        self.balance(&actor.token_a),
                        self.balance(&actor.token_b),
                        self.balance(&actor.lp),
                    )
                })
                .collect(),
        }
    }

    fn check_invariants(&self) {
        for (mint, paused) in [(self.mint_a, self.paused_a), (self.mint_b, self.paused_b)] {
            let config = config_pda(&mint);
            let state = self.mint_state(&mint);
            let stored = self.config_state(&mint);

            assert!(state.supply <= stored.supply_cap);
            assert!(stored.supply_cap <= MAX_SUPPLY_CAP);
            // Owners may burn thawed balances through token-2022 without this program,
            // so tracked outstanding supply only bounds the live supply from above.
            assert!(stored.total_minted >= stored.total_burned);
            assert!(stored.total_minted - stored.total_burned >= u128::from(state.supply));
            assert_eq!(Option::from(state.mint_authority), Some(config));
            assert_eq!(Option::from(state.freeze_authority), Some(config));

            let pausable = self.pausable(&mint);
            assert_eq!(Option::<Pubkey>::from(pausable.authority), Some(config));
            assert_eq!(bool::from(pausable.paused), stored.paused);
            assert_eq!(stored.paused, paused);

            let role = self
                .svm
                .get_account(&minter_pda(&config, &self.admin.pubkey()))
                .expect("minter role missing");
            let role = MinterRole::try_deserialize(&mut role.data.as_slice())
                .expect("role deserialize failed");
            assert!(role.minted <= role.allowance);
        }

        let mut policies = vec![
            (self.mint_a, self.vault_a, true),
            (self.mint_b, self.vault_b, true),
        ];
        for actor in &self.actors {
            policies.push((self.mint_a, actor.token_a, actor.allowed_a));
            policies.push((self.mint_b, actor.token_b, actor.allowed_b));
        }
        for (mint, token_account, allowed) in policies {
            let state = self.token_state(&token_account);
            assert_eq!(state.state == AccountState::Frozen, !allowed);
            assert!(self.has_pausable_account(&token_account));
            let policy = self
                .svm
                .get_account(&policy_pda(&mint, &token_account))
                .expect("policy missing");
            let policy =
                stablecoin::state::WalletPolicy::try_deserialize(&mut policy.data.as_slice())
                    .expect("policy deserialize failed");
            assert_eq!(policy.status == PolicyStatus::Allowed, allowed);
        }

        let pool = self.pool_state();
        assert_eq!(pool.mint_a, self.mint_a);
        assert_eq!(pool.mint_b, self.mint_b);
        assert!(pool.mint_a.to_bytes() < pool.mint_b.to_bytes());
        assert_eq!(pool.fee_bps, FEE_BPS);
        assert_eq!(pool.decimals, DECIMALS);
        assert!(self.balance(&self.vault_a) >= pool.reserve_a);
        assert!(self.balance(&self.vault_b) >= pool.reserve_b);
        assert_eq!(pool.reserve_a == 0, pool.reserve_b == 0);

        let supply = self.lp_supply();
        let locked = self.balance(&self.locked_lp);
        let tracked: u64 = self
            .actors
            .iter()
            .map(|actor| self.balance(&actor.lp))
            .sum::<u64>()
            + locked;
        assert_eq!(supply, tracked);
        assert!(supply >= locked);
        assert!(supply == 0 || locked >= MINIMUM_LIQUIDITY);
        assert_eq!(supply == 0, pool.reserve_a == 0);

        let lp_mint = self.mint_state(&self.lp_mint);
        assert_eq!(lp_mint.decimals, pool.decimals);
        assert_eq!(Option::from(lp_mint.mint_authority), Some(self.pool));
        assert_eq!(Option::<Pubkey>::from(lp_mint.freeze_authority), None);
        assert_eq!(self.token_state(&self.locked_lp).owner, self.pool);
        assert_eq!(self.token_state(&self.locked_lp).mint, self.lp_mint);
        assert_eq!(self.token_state(&self.vault_a).owner, self.pool);
        assert_eq!(self.token_state(&self.vault_b).owner, self.pool);
    }
}

/// `k_new * L_old^2 >= k_old * L_new^2` keeps normalized pool value non-decreasing
/// without floating point.
fn assert_value_not_extracted(before: &Snapshot, after: &Snapshot) {
    let product = |snapshot: &Snapshot| {
        u128::from(snapshot.reserve_a)
            .checked_mul(u128::from(snapshot.reserve_b))
            .expect("k fits in u128")
    };
    let squared = |snapshot: &Snapshot| {
        u128::from(snapshot.lp_supply)
            .checked_mul(u128::from(snapshot.lp_supply))
            .expect("lp supply squared fits in u128")
    };
    let left = product(after)
        .checked_mul(squared(before))
        .expect("normalized value fits in u128");
    let right = product(before)
        .checked_mul(squared(after))
        .expect("normalized value fits in u128");
    assert!(
        left >= right,
        "normalized pool value decreased: {left} < {right}"
    );
}

fn custom_code(result: &TxResult) -> Option<u32> {
    match result {
        Ok(()) => None,
        Err(failure) => match failure.err {
            TransactionError::InstructionError(_, InstructionError::Custom(code)) => Some(code),
            _ => None,
        },
    }
}

#[test]
fn deterministic_operation_sequence_preserves_every_invariant() {
    let mut world = World::new();
    let mut rng = Rng::new(0x5eed_1234_abcd_0001);
    world.check_invariants();

    let mut succeeded = 0usize;
    let mut rejected = 0usize;
    let mut deliberate_rejections = 0usize;
    let mut direct_burns = 0usize;
    let mut swaps = 0usize;
    let mut withdrawals = 0usize;
    let mut codes: Vec<Option<u32>> = Vec::new();

    for index in 0..OPERATIONS {
        // Midway through, an owner burns through token-2022 directly. The program never
        // sees it, so its counters must not move while the live supply drops.
        if index == OPERATIONS / 2 {
            let (mint, token_account) = (world.mint_a, world.actors[0].token_a);
            if world.paused_a {
                world.set_pause(mint, false).expect("resume failed");
                world.paused_a = false;
            }
            if !world.actors[0].allowed_a {
                world
                    .set_policy(mint, token_account, PolicyStatus::Allowed)
                    .expect("unblocking failed");
                world.actors[0].allowed_a = true;
            }
            let before = world.snapshot();
            let amount = before.balances[0].0 / 4;
            assert!(amount > 0, "the owner needs a balance to burn");
            world.direct_burn(0, mint, token_account, amount);
            let after = world.snapshot();
            assert_eq!(after.supply_a, before.supply_a - amount);
            assert_eq!(after.balances[0].0, before.balances[0].0 - amount);
            assert_eq!(after.counters_a, before.counters_a);
            direct_burns += 1;
            world.check_invariants();
            continue;
        }

        let before = world.snapshot();
        let actor = usize::try_from(rng.below(ACTORS as u64)).expect("actor index fits");
        let choice = rng.below(10);

        let (result, expected) = match choice {
            0 | 1 => {
                // Proportional deposit: quoting pulls the matching side itself.
                let amount_a = rng.between(50_000, 400_000);
                let amount_b = before
                    .reserve_b
                    .saturating_mul(amount_a)
                    .checked_div(before.reserve_a.max(1))
                    .unwrap_or(0)
                    .saturating_add(2);
                (world.add_liquidity(actor, amount_a, amount_b), None)
            }
            2 => {
                let amount_a = rng.between(50_000, 400_000);
                let amount_b = rng.between(50_000, 4_000_000);
                (world.add_liquidity(actor, amount_a, amount_b), None)
            }
            3 | 4 => {
                let held = before.balances[actor].2;
                let removable = held.min(before.lp_supply.saturating_sub(before.locked_lp));
                if removable == 0 {
                    continue;
                }
                withdrawals += 1;
                let lp_amount = rng.between(1, removable);
                let result = world.remove_liquidity(actor, lp_amount);
                if result.is_ok() {
                    let after = world.snapshot();
                    let out_a = u128::from(after.balances[actor].0 - before.balances[actor].0);
                    let out_b = u128::from(after.balances[actor].1 - before.balances[actor].1);
                    let share = u128::from(lp_amount);
                    let supply = u128::from(before.lp_supply);
                    assert!(out_a * supply <= share * u128::from(before.reserve_a));
                    assert!(out_b * supply <= share * u128::from(before.reserve_b));
                }
                (result, None)
            }
            5 | 6 => {
                let direction = if rng.below(2) == 0 {
                    SwapDirection::AtoB
                } else {
                    SwapDirection::BtoA
                };
                let amount_in = rng.between(1_000, 200_000);
                swaps += 1;
                let result = world.swap(actor, direction, amount_in);
                if result.is_ok() {
                    let after = world.snapshot();
                    let old_k = u128::from(before.reserve_a) * u128::from(before.reserve_b);
                    let new_k = u128::from(after.reserve_a) * u128::from(after.reserve_b);
                    assert!(new_k >= old_k, "swap decreased k");
                }
                (result, None)
            }
            7 => {
                // Resume takes priority so the protocol never stays paused for long.
                let (mint, paused) = if world.paused_a {
                    (world.mint_a, true)
                } else if world.paused_b {
                    (world.mint_b, true)
                } else if rng.below(2) == 0 {
                    (world.mint_a, false)
                } else {
                    (world.mint_b, false)
                };
                let result = world.set_pause(mint, !paused);
                if result.is_ok() {
                    if mint == world.mint_a {
                        world.paused_a = !paused;
                    } else {
                        world.paused_b = !paused;
                    }
                }
                (result, None)
            }
            8 => {
                // Restoring access takes priority for the same reason as resuming.
                let use_b = if world.actors[actor].allowed_a && !world.actors[actor].allowed_b {
                    true
                } else if !world.actors[actor].allowed_a {
                    false
                } else {
                    rng.below(2) == 1
                };
                let (mint, token_account, allowed) = if use_b {
                    (
                        world.mint_b,
                        world.actors[actor].token_b,
                        world.actors[actor].allowed_b,
                    )
                } else {
                    (
                        world.mint_a,
                        world.actors[actor].token_a,
                        world.actors[actor].allowed_a,
                    )
                };
                let status = if allowed {
                    PolicyStatus::Blocked
                } else {
                    PolicyStatus::Allowed
                };
                let result = world.set_policy(mint, token_account, status);
                if result.is_ok() {
                    if use_b {
                        world.actors[actor].allowed_b = !allowed;
                    } else {
                        world.actors[actor].allowed_a = !allowed;
                    }
                }
                (result, None)
            }
            _ => {
                // Deliberately rejected. Policy is an account constraint, so it is checked
                // before the handler reads the pause flags.
                let blocked = !world.actors[actor].allowed_a || !world.actors[actor].allowed_b;
                let expected = if blocked {
                    AmmError::WalletNotAllowed
                } else if world.paused_a || world.paused_b {
                    AmmError::ProtocolPaused
                } else {
                    let mint = world.mint_a;
                    let account = world.actors[actor].token_a;
                    world
                        .set_policy(mint, account, PolicyStatus::Blocked)
                        .expect("blocking failed");
                    world.actors[actor].allowed_a = false;
                    AmmError::WalletNotAllowed
                };
                deliberate_rejections += 1;
                let result = world.swap(actor, SwapDirection::AtoB, 10_000);
                if u32::from(expected) == u32::from(AmmError::WalletNotAllowed) {
                    let (mint_a, mint_b) = (world.mint_a, world.mint_b);
                    let token_a = world.actors[actor].token_a;
                    let token_b = world.actors[actor].token_b;
                    world
                        .set_policy(mint_a, token_a, PolicyStatus::Allowed)
                        .expect("unblocking failed");
                    world
                        .set_policy(mint_b, token_b, PolicyStatus::Allowed)
                        .expect("unblocking failed");
                    world.actors[actor].allowed_a = true;
                    world.actors[actor].allowed_b = true;
                }
                (result, Some(expected))
            }
        };

        let after = world.snapshot();
        if let Some(expected) = expected {
            assert_eq!(
                custom_code(&result),
                Some(u32::from(expected)),
                "expected rejection did not produce its error"
            );
        }
        if result.is_ok() {
            succeeded += 1;
            assert_value_not_extracted(&before, &after);
        } else {
            rejected += 1;
            codes.push(custom_code(&result));
            assert_eq!(before, after, "a rejected operation changed state");
        }
        world.check_invariants();
    }

    assert!(swaps > 0 && withdrawals > 0 && deliberate_rejections > 0);
    assert!(direct_burns > 0);
    assert!(succeeded > 0 && rejected > 0);
    assert!(
        succeeded + rejected >= 128,
        "only {} operations were attempted",
        succeeded + rejected
    );
    println!(
        "attempted={} succeeded={succeeded} rejected={rejected} swaps={swaps} \
         withdrawals={withdrawals} deliberate_rejections={deliberate_rejections} \
         direct_burns={direct_burns}",
        succeeded + rejected
    );
    assert!(
        codes
            .iter()
            .all(|code| *code == Some(u32::from(AmmError::ProtocolPaused))
                || *code == Some(u32::from(AmmError::WalletNotAllowed))),
        "an operation failed for an unexpected reason: {codes:?}"
    );

    // Every provider withdraws everything that is not permanently locked.
    if world.paused_a {
        let mint = world.mint_a;
        world.set_pause(mint, false).expect("resume failed");
        world.paused_a = false;
    }
    if world.paused_b {
        let mint = world.mint_b;
        world.set_pause(mint, false).expect("resume failed");
        world.paused_b = false;
    }
    for index in 0..ACTORS {
        let (mint_a, mint_b) = (world.mint_a, world.mint_b);
        let (token_a, token_b) = (world.actors[index].token_a, world.actors[index].token_b);
        if !world.actors[index].allowed_a {
            world
                .set_policy(mint_a, token_a, PolicyStatus::Allowed)
                .expect("unblocking failed");
            world.actors[index].allowed_a = true;
        }
        if !world.actors[index].allowed_b {
            world
                .set_policy(mint_b, token_b, PolicyStatus::Allowed)
                .expect("unblocking failed");
            world.actors[index].allowed_b = true;
        }
        let held = world.balance(&world.actors[index].lp);
        let locked = world.balance(&world.locked_lp);
        let removable = held.min(world.lp_supply() - locked);
        if removable > 0 {
            world
                .remove_liquidity(index, removable)
                .expect("final withdrawal failed");
        }
        world.check_invariants();
    }

    let pool = world.pool_state();
    assert_eq!(world.lp_supply(), world.balance(&world.locked_lp));
    assert!(world.balance(&world.vault_a) >= pool.reserve_a);
    assert!(world.balance(&world.vault_b) >= pool.reserve_b);
    assert!(pool.reserve_a > 0 && pool.reserve_b > 0);
}
