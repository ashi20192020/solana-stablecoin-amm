use anchor_lang::prelude::*;
use anchor_spl::{
    token_2022::{burn_checked, BurnChecked, Token2022},
    token_interface::{Mint, TokenAccount},
};

use crate::{
    constants::CONFIG_SEED, errors::StablecoinError, events::TokensBurned,
    instructions::ensure_supply_tracked, state::MintConfig,
};

#[derive(Accounts)]
pub struct Burn<'info> {
    pub owner: Signer<'info>,

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
        owner = anchor_spl::token_2022::ID,
        constraint = source.mint == mint.key() @ StablecoinError::TokenAccountMintMismatch,
        constraint = source.owner == owner.key() @ StablecoinError::Unauthorized,
    )]
    pub source: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token2022>,
}

pub(crate) fn handler(ctx: Context<Burn>, amount: u64) -> Result<()> {
    require!(amount > 0, StablecoinError::ZeroAmount);
    require!(!ctx.accounts.config.paused, StablecoinError::ProtocolPaused);

    let config = &ctx.accounts.config;
    let supply = ctx.accounts.mint.supply;
    ensure_supply_tracked(config, supply)?;

    let total_burned = config
        .total_burned
        .checked_add(u128::from(amount))
        .ok_or(StablecoinError::MathOverflow)?;

    burn_checked(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            BurnChecked {
                mint: ctx.accounts.mint.to_account_info(),
                from: ctx.accounts.source.to_account_info(),
                authority: ctx.accounts.owner.to_account_info(),
            },
        ),
        amount,
        ctx.accounts.mint.decimals,
    )?;

    // Computed after the CPI so Token-2022 keeps reporting insufficient balances itself.
    let new_supply = supply
        .checked_sub(amount)
        .ok_or(StablecoinError::MathOverflow)?;
    ctx.accounts.mint.reload()?;
    require!(
        ctx.accounts.mint.supply == new_supply,
        StablecoinError::CounterInvariantViolation
    );

    ctx.accounts.config.total_burned = total_burned;
    ensure_supply_tracked(&ctx.accounts.config, new_supply)?;

    emit!(TokensBurned {
        mint: ctx.accounts.mint.key(),
        owner: ctx.accounts.owner.key(),
        source: ctx.accounts.source.key(),
        amount,
        supply: new_supply,
    });

    Ok(())
}
