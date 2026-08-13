use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::{mint_to_checked, MintToChecked, Token2022},
    token_interface::{Mint, TokenAccount},
};

use crate::{
    constants::{CONFIG_SEED, MINTER_SEED, POLICY_SEED},
    errors::StablecoinError,
    events::TokensMinted,
    state::{MintConfig, MinterRole, PolicyStatus, WalletPolicy},
};

#[derive(Accounts)]
pub struct MintTo<'info> {
    pub minter: Signer<'info>,

    #[account(mut, owner = anchor_spl::token_2022::ID)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [CONFIG_SEED, mint.key().as_ref()],
        bump = config.bump,
        has_one = mint,
    )]
    pub config: Account<'info, MintConfig>,

    #[account(
        mut,
        seeds = [MINTER_SEED, config.key().as_ref(), minter.key().as_ref()],
        bump = minter_role.bump,
        has_one = config @ StablecoinError::Unauthorized,
        constraint = minter_role.authority == minter.key() @ StablecoinError::Unauthorized,
    )]
    pub minter_role: Account<'info, MinterRole>,

    #[account(
        mut,
        owner = anchor_spl::token_2022::ID,
        constraint = destination.mint == mint.key() @ StablecoinError::TokenAccountMintMismatch,
    )]
    pub destination: InterfaceAccount<'info, TokenAccount>,

    #[account(
        seeds = [POLICY_SEED, mint.key().as_ref(), destination.key().as_ref()],
        bump = wallet_policy.bump,
        has_one = mint,
    )]
    pub wallet_policy: Account<'info, WalletPolicy>,

    pub token_program: Program<'info, Token2022>,
}

pub(crate) fn handler(ctx: Context<MintTo>, amount: u64) -> Result<()> {
    require!(amount > 0, StablecoinError::ZeroAmount);
    require!(!ctx.accounts.config.paused, StablecoinError::ProtocolPaused);
    require!(
        ctx.accounts.wallet_policy.status == PolicyStatus::Allowed,
        StablecoinError::WalletNotAllowed
    );

    let config = &ctx.accounts.config;
    let supply = ctx.accounts.mint.supply;
    require!(
        config.total_minted.checked_sub(config.total_burned) == Some(u128::from(supply)),
        StablecoinError::CounterInvariantViolation
    );

    let minted = ctx
        .accounts
        .minter_role
        .minted
        .checked_add(amount)
        .ok_or(StablecoinError::MathOverflow)?;
    require!(
        minted <= ctx.accounts.minter_role.allowance,
        StablecoinError::AllowanceExceeded
    );

    let new_supply = supply
        .checked_add(amount)
        .ok_or(StablecoinError::MathOverflow)?;
    require!(
        new_supply <= config.supply_cap,
        StablecoinError::SupplyCapExceeded
    );

    let total_minted = config
        .total_minted
        .checked_add(u128::from(amount))
        .ok_or(StablecoinError::MathOverflow)?;

    let config_seeds: &[&[u8]] = &[CONFIG_SEED, config.mint.as_ref(), &[config.bump]];
    mint_to_checked(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            MintToChecked {
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.destination.to_account_info(),
                authority: config.to_account_info(),
            },
            &[config_seeds],
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    ctx.accounts.mint.reload()?;
    require!(
        ctx.accounts.mint.supply == new_supply,
        StablecoinError::CounterInvariantViolation
    );

    ctx.accounts.minter_role.minted = minted;
    ctx.accounts.config.total_minted = total_minted;

    emit!(TokensMinted {
        mint: ctx.accounts.mint.key(),
        minter: ctx.accounts.minter.key(),
        destination: ctx.accounts.destination.key(),
        amount,
        minter_minted: minted,
        supply: new_supply,
    });

    Ok(())
}
