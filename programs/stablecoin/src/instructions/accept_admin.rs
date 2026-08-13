use anchor_lang::prelude::*;

use crate::{
    constants::CONFIG_SEED, errors::StablecoinError, events::AdminAccepted, state::MintConfig,
};

#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    pub pending_admin: Signer<'info>,

    #[account(
        mut,
        seeds = [CONFIG_SEED, config.mint.as_ref()],
        bump = config.bump,
        constraint = config.pending_admin == Some(pending_admin.key())
            @ StablecoinError::NoPendingAdmin,
    )]
    pub config: Account<'info, MintConfig>,
}

pub(crate) fn handler(ctx: Context<AcceptAdmin>) -> Result<()> {
    let admin = ctx.accounts.pending_admin.key();
    let config = &mut ctx.accounts.config;
    let previous_admin = config.admin;
    config.admin = admin;
    config.pending_admin = None;

    emit!(AdminAccepted {
        mint: config.mint,
        previous_admin,
        admin,
    });

    Ok(())
}
