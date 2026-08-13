use anchor_lang::prelude::*;

use crate::state::PolicyStatus;

#[event]
pub struct StablecoinInitialized {
    pub mint: Pubkey,
    pub config: Pubkey,
    pub admin: Pubkey,
    pub symbol: [u8; 8],
    pub decimals: u8,
    pub supply_cap: u64,
}

#[event]
pub struct MinterGranted {
    pub config: Pubkey,
    pub authority: Pubkey,
    pub allowance: u64,
}

#[event]
pub struct MinterRevoked {
    pub config: Pubkey,
    pub authority: Pubkey,
    pub minted: u64,
}

#[event]
pub struct WalletPolicyChanged {
    pub mint: Pubkey,
    pub token_account: Pubkey,
    pub owner: Pubkey,
    pub status: PolicyStatus,
    pub updated_by: Pubkey,
    pub updated_at: i64,
}

#[event]
pub struct TokensMinted {
    pub mint: Pubkey,
    pub minter: Pubkey,
    pub destination: Pubkey,
    pub amount: u64,
    pub minter_minted: u64,
    pub supply: u64,
}

#[event]
pub struct TokensBurned {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub source: Pubkey,
    pub amount: u64,
    pub supply: u64,
}

#[event]
pub struct PauseChanged {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub paused: bool,
}

#[event]
pub struct ConfigUpdated {
    pub mint: Pubkey,
    pub admin: Pubkey,
    pub supply_cap: u64,
    pub compliance_authority: Pubkey,
    pub pending_admin: Option<Pubkey>,
}

#[event]
pub struct AdminAccepted {
    pub mint: Pubkey,
    pub previous_admin: Pubkey,
    pub admin: Pubkey,
}
