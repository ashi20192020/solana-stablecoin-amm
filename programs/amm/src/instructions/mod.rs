pub mod add_liquidity;
pub mod initialize_pool;
pub mod remove_liquidity;
pub mod swap;

pub use add_liquidity::*;
pub use initialize_pool::*;
pub use remove_liquidity::*;
pub use swap::*;

use anchor_lang::{prelude::*, solana_program::program_option::COption};
use anchor_spl::token_interface::{Mint, TokenAccount};
use stablecoin::state::MintConfig;

use crate::{constants::MINIMUM_LIQUIDITY, errors::AmmError, state::Pool};

pub(crate) fn ensure_unpaused(config_a: &MintConfig, config_b: &MintConfig) -> Result<()> {
    require!(
        !config_a.paused && !config_b.paused,
        AmmError::ProtocolPaused
    );
    Ok(())
}

/// Vaults may hold donated surplus, so solvency is one-sided: stored reserves must
/// be backed, while any excess is ignored by the pool.
pub(crate) fn ensure_solvent(pool: &Pool, vault_a: u64, vault_b: u64) -> Result<()> {
    require!(
        vault_a >= pool.reserve_a && vault_b >= pool.reserve_b,
        AmmError::VaultBalanceInvariant
    );
    Ok(())
}

/// Anyone can send LP tokens to the locked account, so the locked balance is bounded
/// from below by the minted minimum and from above by the live supply.
pub(crate) fn ensure_seeded(pool: &Pool, lp_supply: u64, locked_lp: u64) -> Result<()> {
    require!(
        pool.reserve_a > 0 && pool.reserve_b > 0 && lp_supply >= MINIMUM_LIQUIDITY,
        AmmError::InvalidLiquidityState
    );
    require!(
        locked_lp >= MINIMUM_LIQUIDITY && locked_lp <= lp_supply,
        AmmError::LockedLiquidityInvariant
    );
    Ok(())
}

/// The stored addresses are canonical, but the live accounts are re-checked so a
/// corrupted or externally mutated child can never be used to move value.
pub(crate) fn ensure_children(
    pool: &Account<'_, Pool>,
    vault_a: &InterfaceAccount<'_, TokenAccount>,
    vault_b: &InterfaceAccount<'_, TokenAccount>,
    lp_mint: &InterfaceAccount<'_, Mint>,
) -> Result<()> {
    let authority = pool.key();
    require_keys_neq!(vault_a.key(), vault_b.key(), AmmError::DuplicateAccount);
    require!(
        vault_a.mint == pool.mint_a && vault_b.mint == pool.mint_b,
        AmmError::TokenAccountMintMismatch
    );
    require!(
        vault_a.owner == authority && vault_b.owner == authority,
        AmmError::InvalidTokenOwner
    );
    require!(
        lp_mint.decimals == pool.decimals,
        AmmError::DecimalsMismatch
    );
    require!(
        lp_mint.mint_authority == COption::Some(authority) && lp_mint.freeze_authority.is_none(),
        AmmError::InvalidMintAuthority
    );
    Ok(())
}

pub(crate) fn ensure_locked_account(
    pool: &Account<'_, Pool>,
    lp_mint: &InterfaceAccount<'_, Mint>,
    locked_lp: &InterfaceAccount<'_, TokenAccount>,
) -> Result<()> {
    require!(
        locked_lp.mint == lp_mint.key(),
        AmmError::TokenAccountMintMismatch
    );
    require!(locked_lp.owner == pool.key(), AmmError::InvalidTokenOwner);
    Ok(())
}
