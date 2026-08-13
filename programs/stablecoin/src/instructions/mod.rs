pub mod accept_admin;
pub mod burn;
pub mod grant_minter;
pub mod initialize_stablecoin;
pub mod mint_to;
pub mod revoke_minter;
pub mod set_pause;
pub mod set_wallet_policy;
pub mod update_config;

use anchor_lang::prelude::*;

use crate::{errors::StablecoinError, state::MintConfig};

pub use accept_admin::*;
pub use burn::*;
pub use grant_minter::*;
pub use initialize_stablecoin::*;
pub use mint_to::*;
pub use revoke_minter::*;
pub use set_pause::*;
pub use set_wallet_policy::*;
pub use update_config::*;

/// `total_minted` covers every issuance, because only the config PDA holds mint
/// authority, but a thawed owner may burn through Token-2022 without this program.
/// Tracked outstanding supply is therefore an upper bound on the live supply, never
/// an equality.
pub(crate) fn ensure_supply_tracked(config: &MintConfig, supply: u64) -> Result<()> {
    let outstanding = config
        .total_minted
        .checked_sub(config.total_burned)
        .ok_or(StablecoinError::CounterInvariantViolation)?;
    require!(
        outstanding >= u128::from(supply),
        StablecoinError::CounterInvariantViolation
    );
    Ok(())
}
