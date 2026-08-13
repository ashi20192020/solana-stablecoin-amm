use anchor_lang::prelude::*;
use anchor_spl::token_interface::Mint;

use crate::{
    constants::{CONFIG_SEED, MAX_SUPPLY_CAP},
    errors::StablecoinError,
    state::MintConfig,
};

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    pub admin: Signer<'info>,

    #[account(owner = anchor_spl::token_2022::ID)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        seeds = [CONFIG_SEED, mint.key().as_ref()],
        bump = config.bump,
        has_one = mint,
        has_one = admin @ StablecoinError::Unauthorized,
    )]
    pub config: Account<'info, MintConfig>,
}

pub(crate) fn handler(
    ctx: Context<UpdateConfig>,
    supply_cap: Option<u64>,
    compliance_authority: Option<Pubkey>,
    pending_admin: Option<Pubkey>,
) -> Result<()> {
    require!(
        supply_cap.is_some() || compliance_authority.is_some() || pending_admin.is_some(),
        StablecoinError::NoConfigChange
    );

    if let Some(cap) = supply_cap {
        require!(cap > 0, StablecoinError::ZeroSupplyCap);
        require!(cap <= MAX_SUPPLY_CAP, StablecoinError::SupplyCapTooLarge);

        let supply = ctx.accounts.mint.supply;
        let config = &ctx.accounts.config;
        require!(
            config.total_minted.checked_sub(config.total_burned) == Some(u128::from(supply)),
            StablecoinError::CounterInvariantViolation
        );
        require!(cap >= supply, StablecoinError::SupplyCapBelowSupply);

        ctx.accounts.config.supply_cap = cap;
    }

    if let Some(authority) = compliance_authority {
        require_keys_neq!(
            authority,
            Pubkey::default(),
            StablecoinError::InvalidAuthority
        );
        ctx.accounts.config.compliance_authority = authority;
    }

    if let Some(authority) = pending_admin {
        require_keys_neq!(
            authority,
            Pubkey::default(),
            StablecoinError::InvalidPendingAdmin
        );
        require_keys_neq!(
            authority,
            ctx.accounts.config.admin,
            StablecoinError::InvalidPendingAdmin
        );
        ctx.accounts.config.pending_admin = Some(authority);
    }

    Ok(())
}
