use anchor_lang::prelude::*;
use anchor_spl::{token_2022::Token2022, token_interface::Mint};

use crate::{
    constants::CONFIG_SEED,
    errors::StablecoinError,
    events::PauseChanged,
    state::MintConfig,
    token2022::{pausable_is_paused, pausable_set},
};

#[derive(Accounts)]
pub struct SetPause<'info> {
    pub authority: Signer<'info>,

    #[account(mut, owner = anchor_spl::token_2022::ID)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [CONFIG_SEED, mint.key().as_ref()],
        bump = config.bump,
        has_one = mint,
        constraint = (config.admin == authority.key()
            || config.compliance_authority == authority.key())
            @ StablecoinError::Unauthorized,
    )]
    pub config: Account<'info, MintConfig>,

    pub token_program: Program<'info, Token2022>,
}

pub(crate) fn handler(ctx: Context<SetPause>, paused: bool) -> Result<()> {
    let mint = ctx.accounts.mint.to_account_info();
    let live = pausable_is_paused(&mint)?;
    require!(
        live == ctx.accounts.config.paused,
        StablecoinError::PauseStateDrift
    );
    require!(live != paused, StablecoinError::PauseStateUnchanged);

    let config = &ctx.accounts.config;
    let config_seeds: &[&[u8]] = &[CONFIG_SEED, config.mint.as_ref(), &[config.bump]];
    pausable_set(
        &ctx.accounts.token_program.to_account_info(),
        &mint,
        &config.to_account_info(),
        paused,
        &[config_seeds],
    )?;

    require!(
        pausable_is_paused(&mint)? == paused,
        StablecoinError::PauseStateDrift
    );
    ctx.accounts.config.paused = paused;

    emit!(PauseChanged {
        mint: ctx.accounts.mint.key(),
        authority: ctx.accounts.authority.key(),
        paused,
    });

    Ok(())
}
