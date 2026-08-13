use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::{mint_to_checked, transfer_checked, MintToChecked, Token2022, TransferChecked},
    token_interface::{Mint, TokenAccount},
};
use stablecoin::{
    constants::{CONFIG_SEED, POLICY_SEED},
    state::{MintConfig, PolicyStatus, WalletPolicy},
};

use crate::{
    constants::POOL_SEED,
    errors::AmmError,
    events::LiquidityAdded,
    instructions::{ensure_seeded, ensure_solvent, ensure_unpaused},
    math::{quote_add_liquidity, quote_initial_liquidity},
    state::Pool,
};

#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.mint_a.as_ref(), pool.mint_b.as_ref()],
        bump = pool.bump,
        has_one = mint_a,
        has_one = mint_b,
        has_one = vault_a,
        has_one = vault_b,
        has_one = lp_mint,
        has_one = locked_lp,
    )]
    pub pool: Box<Account<'info, Pool>>,

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

    #[account(mut, owner = anchor_spl::token_2022::ID)]
    pub vault_a: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, owner = anchor_spl::token_2022::ID)]
    pub vault_b: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(mut, owner = anchor_spl::token_2022::ID)]
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(mut, owner = anchor_spl::token_2022::ID)]
    pub locked_lp: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        owner = anchor_spl::token_2022::ID,
        constraint = user_a.key() != pool.vault_a @ AmmError::DuplicateAccount,
        constraint = user_a.mint == mint_a.key() @ AmmError::TokenAccountMintMismatch,
        constraint = user_a.owner == user.key() @ AmmError::InvalidTokenOwner,
    )]
    pub user_a: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        owner = anchor_spl::token_2022::ID,
        constraint = user_b.key() != pool.vault_b @ AmmError::DuplicateAccount,
        constraint = user_b.mint == mint_b.key() @ AmmError::TokenAccountMintMismatch,
        constraint = user_b.owner == user.key() @ AmmError::InvalidTokenOwner,
    )]
    pub user_b: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        mut,
        owner = anchor_spl::token_2022::ID,
        constraint = user_lp.key() != pool.locked_lp @ AmmError::DuplicateAccount,
        constraint = user_lp.mint == lp_mint.key() @ AmmError::TokenAccountMintMismatch,
        constraint = user_lp.owner == user.key() @ AmmError::InvalidTokenOwner,
    )]
    pub user_lp: Box<InterfaceAccount<'info, TokenAccount>>,

    #[account(
        seeds = [POLICY_SEED, mint_a.key().as_ref(), user_a.key().as_ref()],
        bump = policy_a.bump,
        seeds::program = stablecoin::ID,
        constraint = policy_a.status == PolicyStatus::Allowed @ AmmError::WalletNotAllowed,
    )]
    pub policy_a: Box<Account<'info, WalletPolicy>>,

    #[account(
        seeds = [POLICY_SEED, mint_b.key().as_ref(), user_b.key().as_ref()],
        bump = policy_b.bump,
        seeds::program = stablecoin::ID,
        constraint = policy_b.status == PolicyStatus::Allowed @ AmmError::WalletNotAllowed,
    )]
    pub policy_b: Box<Account<'info, WalletPolicy>>,

    pub token_program: Program<'info, Token2022>,
}

pub(crate) fn handler(
    ctx: Context<AddLiquidity>,
    amount_a_desired: u64,
    amount_b_desired: u64,
    amount_a_min: u64,
    amount_b_min: u64,
    min_lp_out: u64,
) -> Result<()> {
    require!(
        amount_a_desired > 0 && amount_b_desired > 0,
        AmmError::ZeroAmount
    );
    ensure_unpaused(&ctx.accounts.config_a, &ctx.accounts.config_b)?;

    let pool = &ctx.accounts.pool;
    ensure_solvent(
        pool,
        ctx.accounts.vault_a.amount,
        ctx.accounts.vault_b.amount,
        ctx.accounts.lp_mint.supply,
    )?;

    let first_deposit = pool.lp_supply == 0;
    let (amount_a, amount_b, lp_to_user, lp_to_lock) = if first_deposit {
        require!(
            pool.reserve_a == 0 && pool.reserve_b == 0 && ctx.accounts.locked_lp.amount == 0,
            AmmError::InvalidLiquidityState
        );
        let quote =
            quote_initial_liquidity(amount_a_desired, amount_b_desired).map_err(AmmError::from)?;
        (
            amount_a_desired,
            amount_b_desired,
            quote.lp_to_user,
            quote.lp_to_lock,
        )
    } else {
        let quote = quote_add_liquidity(
            amount_a_desired,
            amount_b_desired,
            pool.reserve_a,
            pool.reserve_b,
            pool.lp_supply,
        )
        .map_err(AmmError::from)?;
        (quote.amount_a, quote.amount_b, quote.lp_minted, 0)
    };

    require!(
        amount_a >= amount_a_min && amount_b >= amount_b_min && lp_to_user >= min_lp_out,
        AmmError::SlippageExceeded
    );

    let reserve_a = pool
        .reserve_a
        .checked_add(amount_a)
        .ok_or(AmmError::MathOverflow)?;
    let reserve_b = pool
        .reserve_b
        .checked_add(amount_b)
        .ok_or(AmmError::MathOverflow)?;
    let lp_supply = pool
        .lp_supply
        .checked_add(lp_to_user)
        .and_then(|supply| supply.checked_add(lp_to_lock))
        .ok_or(AmmError::MathOverflow)?;

    let mint_a = pool.mint_a;
    let mint_b = pool.mint_b;
    let pool_seeds: &[&[u8]] = &[POOL_SEED, mint_a.as_ref(), mint_b.as_ref(), &[pool.bump]];
    let token_program = ctx.accounts.token_program.key();
    let decimals = pool.decimals;
    let lp_decimals = ctx.accounts.lp_mint.decimals;
    let pool_info = pool.to_account_info();

    transfer_checked(
        CpiContext::new(
            token_program,
            TransferChecked {
                from: ctx.accounts.user_a.to_account_info(),
                mint: ctx.accounts.mint_a.to_account_info(),
                to: ctx.accounts.vault_a.to_account_info(),
                authority: ctx.accounts.user.to_account_info(),
            },
        ),
        amount_a,
        decimals,
    )?;
    transfer_checked(
        CpiContext::new(
            token_program,
            TransferChecked {
                from: ctx.accounts.user_b.to_account_info(),
                mint: ctx.accounts.mint_b.to_account_info(),
                to: ctx.accounts.vault_b.to_account_info(),
                authority: ctx.accounts.user.to_account_info(),
            },
        ),
        amount_b,
        decimals,
    )?;

    if lp_to_lock > 0 {
        mint_to_checked(
            CpiContext::new_with_signer(
                token_program,
                MintToChecked {
                    mint: ctx.accounts.lp_mint.to_account_info(),
                    to: ctx.accounts.locked_lp.to_account_info(),
                    authority: pool_info.clone(),
                },
                &[pool_seeds],
            ),
            lp_to_lock,
            lp_decimals,
        )?;
    }
    mint_to_checked(
        CpiContext::new_with_signer(
            token_program,
            MintToChecked {
                mint: ctx.accounts.lp_mint.to_account_info(),
                to: ctx.accounts.user_lp.to_account_info(),
                authority: pool_info,
            },
            &[pool_seeds],
        ),
        lp_to_user,
        lp_decimals,
    )?;

    let pool = &mut ctx.accounts.pool;
    pool.reserve_a = reserve_a;
    pool.reserve_b = reserve_b;
    pool.lp_supply = lp_supply;

    ctx.accounts.vault_a.reload()?;
    ctx.accounts.vault_b.reload()?;
    ctx.accounts.lp_mint.reload()?;
    ctx.accounts.locked_lp.reload()?;

    let pool = &ctx.accounts.pool;
    ensure_solvent(
        pool,
        ctx.accounts.vault_a.amount,
        ctx.accounts.vault_b.amount,
        ctx.accounts.lp_mint.supply,
    )?;
    ensure_seeded(pool, ctx.accounts.locked_lp.amount)?;

    emit!(LiquidityAdded {
        pool: pool.key(),
        user: ctx.accounts.user.key(),
        amount_a,
        amount_b,
        lp_minted: lp_to_user,
        reserve_a,
        reserve_b,
        lp_supply,
    });

    Ok(())
}
