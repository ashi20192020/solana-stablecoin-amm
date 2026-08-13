use anchor_lang::prelude::*;

use crate::{
    constants::{CONFIG_SEED, MINTER_SEED},
    errors::StablecoinError,
    state::{MintConfig, MinterRole},
};

#[derive(Accounts)]
pub struct GrantMinter<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [CONFIG_SEED, config.mint.as_ref()],
        bump = config.bump,
        has_one = admin @ StablecoinError::Unauthorized,
    )]
    pub config: Account<'info, MintConfig>,

    /// CHECK: identity only. Its key is bound into the MinterRole PDA seeds and stored
    /// on the role; the account is never read, written, or required to sign.
    pub authority: UncheckedAccount<'info>,

    #[account(
        init,
        payer = admin,
        space = MinterRole::DISCRIMINATOR.len() + MinterRole::INIT_SPACE,
        seeds = [MINTER_SEED, config.key().as_ref(), authority.key().as_ref()],
        bump,
    )]
    pub minter_role: Account<'info, MinterRole>,

    pub system_program: Program<'info, System>,
}

pub(crate) fn handler(ctx: Context<GrantMinter>, allowance: u64) -> Result<()> {
    require!(allowance > 0, StablecoinError::ZeroAllowance);
    let authority = ctx.accounts.authority.key();
    require_keys_neq!(
        authority,
        Pubkey::default(),
        StablecoinError::InvalidAuthority
    );

    ctx.accounts.minter_role.set_inner(MinterRole {
        config: ctx.accounts.config.key(),
        authority,
        allowance,
        minted: 0,
        bump: ctx.bumps.minter_role,
        _reserved: [0; 32],
    });

    Ok(())
}
