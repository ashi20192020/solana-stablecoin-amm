pub mod add_liquidity;
pub mod initialize_pool;
pub mod remove_liquidity;
pub mod swap;

pub use add_liquidity::*;
pub use initialize_pool::*;
pub use remove_liquidity::*;
pub use swap::*;

use anchor_lang::prelude::*;
use stablecoin::state::MintConfig;

use crate::{errors::AmmError, state::Pool};

pub(crate) fn ensure_unpaused(config_a: &MintConfig, config_b: &MintConfig) -> Result<()> {
    require!(
        !config_a.paused && !config_b.paused,
        AmmError::ProtocolPaused
    );
    Ok(())
}

/// Vaults may hold donated surplus, so solvency is one-sided: stored reserves must
/// be backed, while any excess is ignored by the pool.
pub(crate) fn ensure_solvent(
    pool: &Pool,
    vault_a: u64,
    vault_b: u64,
    lp_supply: u64,
) -> Result<()> {
    require!(
        vault_a >= pool.reserve_a && vault_b >= pool.reserve_b,
        AmmError::VaultBalanceInvariant
    );
    require!(lp_supply == pool.lp_supply, AmmError::LpSupplyInvariant);
    Ok(())
}

pub(crate) fn ensure_seeded(pool: &Pool, locked_lp: u64) -> Result<()> {
    require!(
        pool.reserve_a > 0
            && pool.reserve_b > 0
            && pool.lp_supply >= crate::constants::MINIMUM_LIQUIDITY,
        AmmError::InvalidLiquidityState
    );
    require!(
        locked_lp == crate::constants::MINIMUM_LIQUIDITY,
        AmmError::LockedLiquidityInvariant
    );
    Ok(())
}
