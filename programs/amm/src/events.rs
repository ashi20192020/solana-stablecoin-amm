use anchor_lang::prelude::*;

use crate::state::SwapDirection;

#[event]
pub struct PoolInitialized {
    pub pool: Pubkey,
    pub mint_a: Pubkey,
    pub mint_b: Pubkey,
    pub lp_mint: Pubkey,
    pub fee_bps: u16,
    pub decimals: u8,
}

#[event]
pub struct LiquidityAdded {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount_a: u64,
    pub amount_b: u64,
    pub lp_minted: u64,
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub lp_supply: u64,
}

#[event]
pub struct LiquidityRemoved {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub lp_burned: u64,
    pub amount_a: u64,
    pub amount_b: u64,
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub lp_supply: u64,
}

#[event]
pub struct SwapExecuted {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub direction: SwapDirection,
    pub amount_in: u64,
    pub amount_out: u64,
    pub reserve_a: u64,
    pub reserve_b: u64,
}
