use anchor_lang::prelude::*;

use crate::{
    constants::{CONFIG_SEED, MINTER_SEED},
    errors::StablecoinError,
    events::MinterRevoked,
    state::{MintConfig, MinterRole},
};

#[derive(Accounts)]
pub struct RevokeMinter<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED, config.mint.as_ref()],
        bump = config.bump,
        has_one = admin @ StablecoinError::Unauthorized,
    )]
    pub config: Account<'info, MintConfig>,

    /// CHECK: identity only. Its key is bound into the MinterRole PDA seeds and matched
    /// against the stored authority; the account is never read, written, or required to sign.
    pub authority: UncheckedAccount<'info>,

    #[account(
        mut,
        close = admin,
        seeds = [MINTER_SEED, config.key().as_ref(), authority.key().as_ref()],
        bump = minter_role.bump,
        has_one = config @ StablecoinError::Unauthorized,
        has_one = authority @ StablecoinError::Unauthorized,
    )]
    pub minter_role: Account<'info, MinterRole>,
}

pub(crate) fn handler(ctx: Context<RevokeMinter>) -> Result<()> {
    emit!(MinterRevoked {
        config: ctx.accounts.config.key(),
        authority: ctx.accounts.authority.key(),
        minted: ctx.accounts.minter_role.minted,
    });

    Ok(())
}
