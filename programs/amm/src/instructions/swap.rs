use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::{transfer_checked, Token2022, TransferChecked},
    token_interface::{Mint, TokenAccount},
};
use stablecoin::{
    constants::{CONFIG_SEED, POLICY_SEED},
    state::{MintConfig, PolicyStatus, WalletPolicy},
};

use crate::{
    constants::POOL_SEED,
    errors::AmmError,
    events::SwapExecuted,
    instructions::{ensure_solvent, ensure_unpaused},
    math::quote_swap,
    state::{Pool, SwapDirection},
};

#[derive(Accounts)]
pub struct Swap<'info> {
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

    #[account(owner = anchor_spl::token_2022::ID)]
    pub lp_mint: Box<InterfaceAccount<'info, Mint>>,

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
    ctx: Context<Swap>,
    direction: SwapDirection,
    amount_in: u64,
    min_amount_out: u64,
) -> Result<()> {
    require!(amount_in > 0, AmmError::ZeroAmount);
    ensure_unpaused(&ctx.accounts.config_a, &ctx.accounts.config_b)?;

    let pool = &ctx.accounts.pool;
    ensure_solvent(
        pool,
        ctx.accounts.vault_a.amount,
        ctx.accounts.vault_b.amount,
    )?;

    let (reserve_in, reserve_out) = match direction {
        SwapDirection::AtoB => (pool.reserve_a, pool.reserve_b),
        SwapDirection::BtoA => (pool.reserve_b, pool.reserve_a),
    };
    let old_k = u128::from(reserve_in)
        .checked_mul(u128::from(reserve_out))
        .ok_or(AmmError::MathOverflow)?;

    let quote =
        quote_swap(amount_in, reserve_in, reserve_out, pool.fee_bps).map_err(AmmError::from)?;
    require!(
        quote.amount_out >= min_amount_out,
        AmmError::SlippageExceeded
    );

    let mint_a = pool.mint_a;
    let mint_b = pool.mint_b;
    let pool_seeds: &[&[u8]] = &[POOL_SEED, mint_a.as_ref(), mint_b.as_ref(), &[pool.bump]];
    let token_program = ctx.accounts.token_program.key();
    let decimals = pool.decimals;
    let pool_info = pool.to_account_info();

    let (source, source_mint, vault_in, vault_out, destination_mint, destination) = match direction
    {
        SwapDirection::AtoB => (
            ctx.accounts.user_a.to_account_info(),
            ctx.accounts.mint_a.to_account_info(),
            ctx.accounts.vault_a.to_account_info(),
            ctx.accounts.vault_b.to_account_info(),
            ctx.accounts.mint_b.to_account_info(),
            ctx.accounts.user_b.to_account_info(),
        ),
        SwapDirection::BtoA => (
            ctx.accounts.user_b.to_account_info(),
            ctx.accounts.mint_b.to_account_info(),
            ctx.accounts.vault_b.to_account_info(),
            ctx.accounts.vault_a.to_account_info(),
            ctx.accounts.mint_a.to_account_info(),
            ctx.accounts.user_a.to_account_info(),
        ),
    };

    transfer_checked(
        CpiContext::new(
            token_program,
            TransferChecked {
                from: source,
                mint: source_mint,
                to: vault_in,
                authority: ctx.accounts.user.to_account_info(),
            },
        ),
        amount_in,
        decimals,
    )?;
    transfer_checked(
        CpiContext::new_with_signer(
            token_program,
            TransferChecked {
                from: vault_out,
                mint: destination_mint,
                to: destination,
                authority: pool_info,
            },
            &[pool_seeds],
        ),
        quote.amount_out,
        decimals,
    )?;

    let (reserve_a, reserve_b) = match direction {
        SwapDirection::AtoB => (quote.new_reserve_in, quote.new_reserve_out),
        SwapDirection::BtoA => (quote.new_reserve_out, quote.new_reserve_in),
    };
    let new_k = u128::from(quote.new_reserve_in)
        .checked_mul(u128::from(quote.new_reserve_out))
        .ok_or(AmmError::MathOverflow)?;
    require!(new_k >= old_k, AmmError::ConstantProductViolation);

    let pool = &mut ctx.accounts.pool;
    pool.reserve_a = reserve_a;
    pool.reserve_b = reserve_b;

    ctx.accounts.vault_a.reload()?;
    ctx.accounts.vault_b.reload()?;

    let pool = &ctx.accounts.pool;
    ensure_solvent(
        pool,
        ctx.accounts.vault_a.amount,
        ctx.accounts.vault_b.amount,
    )?;

    emit!(SwapExecuted {
        pool: pool.key(),
        user: ctx.accounts.user.key(),
        direction,
        amount_in,
        amount_out: quote.amount_out,
        reserve_a,
        reserve_b,
    });

    Ok(())
}
