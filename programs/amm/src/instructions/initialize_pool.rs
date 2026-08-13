use anchor_lang::{
    prelude::*,
    system_program::{create_account, CreateAccount},
};
use anchor_spl::{
    token_2022::{
        initialize_account3, initialize_mint2,
        spl_token_2022::{
            extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions},
            state::{Account as TokenAccountState, Mint as MintState},
        },
        InitializeAccount3, InitializeMint2, Token2022,
    },
    token_interface::Mint,
};
use stablecoin::{
    constants::{CONFIG_SEED, STABLECOIN_DECIMALS},
    state::MintConfig,
};

use crate::{
    constants::{LOCKED_LP_SEED, LP_MINT_SEED, MAX_FEE_BPS, POOL_SEED, VAULT_SEED},
    errors::AmmError,
    events::PoolInitialized,
    state::Pool,
};

const SUPPORTED_MINT_EXTENSIONS: [ExtensionType; 4] = [
    ExtensionType::DefaultAccountState,
    ExtensionType::Pausable,
    ExtensionType::MetadataPointer,
    ExtensionType::TokenMetadata,
];

#[derive(Accounts)]
pub struct InitializePool<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(owner = anchor_spl::token_2022::ID)]
    pub mint_a: Box<InterfaceAccount<'info, Mint>>,

    #[account(owner = anchor_spl::token_2022::ID)]
    pub mint_b: Box<InterfaceAccount<'info, Mint>>,

    #[account(
        seeds = [CONFIG_SEED, mint_a.key().as_ref()],
        bump = config_a.bump,
        seeds::program = stablecoin::ID,
        constraint = config_a.mint == mint_a.key() @ AmmError::InvalidStablecoinConfig,
    )]
    pub config_a: Box<Account<'info, MintConfig>>,

    #[account(
        seeds = [CONFIG_SEED, mint_b.key().as_ref()],
        bump = config_b.bump,
        seeds::program = stablecoin::ID,
        constraint = config_b.mint == mint_b.key() @ AmmError::InvalidStablecoinConfig,
    )]
    pub config_b: Box<Account<'info, MintConfig>>,

    #[account(
        init,
        payer = payer,
        space = Pool::DISCRIMINATOR.len() + Pool::INIT_SPACE,
        seeds = [POOL_SEED, mint_a.key().as_ref(), mint_b.key().as_ref()],
        bump,
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// CHECK: allocated and initialized below as the canonical Token-2022 vault for `mint_a`.
    #[account(mut, seeds = [VAULT_SEED, pool.key().as_ref(), mint_a.key().as_ref()], bump)]
    pub vault_a: UncheckedAccount<'info>,

    /// CHECK: allocated and initialized below as the canonical Token-2022 vault for `mint_b`.
    #[account(mut, seeds = [VAULT_SEED, pool.key().as_ref(), mint_b.key().as_ref()], bump)]
    pub vault_b: UncheckedAccount<'info>,

    /// CHECK: allocated and initialized below as the canonical extension-free LP mint.
    #[account(mut, seeds = [LP_MINT_SEED, pool.key().as_ref()], bump)]
    pub lp_mint: UncheckedAccount<'info>,

    /// CHECK: allocated and initialized below as the pool-owned locked LP account.
    #[account(mut, seeds = [LOCKED_LP_SEED, pool.key().as_ref()], bump)]
    pub locked_lp: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(ctx: Context<InitializePool>, fee_bps: u16) -> Result<()> {
    let mint_a = ctx.accounts.mint_a.key();
    let mint_b = ctx.accounts.mint_b.key();
    require_keys_neq!(mint_a, mint_b, AmmError::IdenticalMints);
    require!(
        mint_a.as_ref() < mint_b.as_ref(),
        AmmError::InvalidMintOrder
    );
    require!(fee_bps <= MAX_FEE_BPS, AmmError::FeeTooHigh);

    let decimals = ctx.accounts.mint_a.decimals;
    require!(
        decimals == ctx.accounts.mint_b.decimals && decimals == STABLECOIN_DECIMALS,
        AmmError::DecimalsMismatch
    );

    let pool_key = ctx.accounts.pool.key();
    let token_program = ctx.accounts.token_program.key();
    let system_program = ctx.accounts.system_program.key();
    let payer = ctx.accounts.payer.to_account_info();
    let pool_info = ctx.accounts.pool.to_account_info();
    let bumps = &ctx.bumps;

    create_vault(
        &payer,
        &ctx.accounts.vault_a.to_account_info(),
        &ctx.accounts.mint_a.to_account_info(),
        &pool_info,
        system_program,
        token_program,
        &[
            VAULT_SEED,
            pool_key.as_ref(),
            mint_a.as_ref(),
            &[bumps.vault_a],
        ],
    )?;
    create_vault(
        &payer,
        &ctx.accounts.vault_b.to_account_info(),
        &ctx.accounts.mint_b.to_account_info(),
        &pool_info,
        system_program,
        token_program,
        &[
            VAULT_SEED,
            pool_key.as_ref(),
            mint_b.as_ref(),
            &[bumps.vault_b],
        ],
    )?;

    create_pda_account(
        &payer,
        &ctx.accounts.lp_mint.to_account_info(),
        system_program,
        &token_program,
        ExtensionType::try_calculate_account_len::<MintState>(&[])?,
        &[LP_MINT_SEED, pool_key.as_ref(), &[bumps.lp_mint]],
    )?;
    initialize_mint2(
        CpiContext::new(
            token_program,
            InitializeMint2 {
                mint: ctx.accounts.lp_mint.to_account_info(),
            },
        ),
        decimals,
        &pool_key,
        None,
    )?;

    create_pda_account(
        &payer,
        &ctx.accounts.locked_lp.to_account_info(),
        system_program,
        &token_program,
        ExtensionType::try_calculate_account_len::<TokenAccountState>(&[])?,
        &[LOCKED_LP_SEED, pool_key.as_ref(), &[bumps.locked_lp]],
    )?;
    initialize_account3(CpiContext::new(
        token_program,
        InitializeAccount3 {
            account: ctx.accounts.locked_lp.to_account_info(),
            mint: ctx.accounts.lp_mint.to_account_info(),
            authority: pool_info,
        },
    ))?;

    let lp_mint = ctx.accounts.lp_mint.key();
    ctx.accounts.pool.set_inner(Pool {
        mint_a,
        mint_b,
        vault_a: ctx.accounts.vault_a.key(),
        vault_b: ctx.accounts.vault_b.key(),
        lp_mint,
        locked_lp: ctx.accounts.locked_lp.key(),
        reserve_a: 0,
        reserve_b: 0,
        lp_supply: 0,
        fee_bps,
        decimals,
        bump: bumps.pool,
        vault_a_bump: bumps.vault_a,
        vault_b_bump: bumps.vault_b,
        lp_mint_bump: bumps.lp_mint,
        locked_lp_bump: bumps.locked_lp,
        _reserved: [0; 64],
    });

    emit!(PoolInitialized {
        pool: pool_key,
        mint_a,
        mint_b,
        lp_mint,
        fee_bps,
        decimals,
    });

    Ok(())
}

fn create_vault<'info>(
    payer: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    pool: &AccountInfo<'info>,
    system_program: Pubkey,
    token_program: Pubkey,
    seeds: &[&[u8]],
) -> Result<()> {
    let space = vault_space(mint)?;
    create_pda_account(payer, vault, system_program, &token_program, space, seeds)?;
    // Token-2022 initializes the mint-required account extensions and applies the
    // mint's default frozen state during this call, so vaults start frozen.
    initialize_account3(CpiContext::new(
        token_program,
        InitializeAccount3 {
            account: vault.clone(),
            mint: mint.clone(),
            authority: pool.clone(),
        },
    ))
}

fn vault_space(mint: &AccountInfo<'_>) -> Result<usize> {
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<MintState>::unpack(&data)?;
    let extensions = state.get_extension_types()?;
    require!(
        extensions
            .iter()
            .all(|extension| SUPPORTED_MINT_EXTENSIONS.contains(extension)),
        AmmError::UnsupportedMintExtension
    );

    let required = ExtensionType::get_required_init_account_extensions(&extensions);
    ExtensionType::try_calculate_account_len::<TokenAccountState>(&required).map_err(Into::into)
}

fn create_pda_account<'info>(
    payer: &AccountInfo<'info>,
    target: &AccountInfo<'info>,
    system_program: Pubkey,
    owner: &Pubkey,
    space: usize,
    seeds: &[&[u8]],
) -> Result<()> {
    create_account(
        CpiContext::new(
            system_program,
            CreateAccount {
                from: payer.clone(),
                to: target.clone(),
            },
        )
        .with_signer(&[seeds]),
        Rent::get()?.minimum_balance(space),
        u64::try_from(space).map_err(|_| AmmError::MathOverflow)?,
        owner,
    )
}
