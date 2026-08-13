use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod events;
pub mod instructions;
pub mod math;
pub mod state;

use instructions::*;
use state::SwapDirection;

declare_id!("F8JkSibw1r9bfqj1XGaomozVbTq5YPg7L7zqJWKv7evU");

#[program]
pub mod amm {
    use super::*;

    pub fn initialize_pool(ctx: Context<InitializePool>, fee_bps: u16) -> Result<()> {
        instructions::initialize_pool::handler(ctx, fee_bps)
    }

    pub fn add_liquidity(
        ctx: Context<AddLiquidity>,
        amount_a_desired: u64,
        amount_b_desired: u64,
        amount_a_min: u64,
        amount_b_min: u64,
        min_lp_out: u64,
    ) -> Result<()> {
        instructions::add_liquidity::handler(
            ctx,
            amount_a_desired,
            amount_b_desired,
            amount_a_min,
            amount_b_min,
            min_lp_out,
        )
    }

    pub fn remove_liquidity(
        ctx: Context<RemoveLiquidity>,
        lp_amount: u64,
        min_a_out: u64,
        min_b_out: u64,
    ) -> Result<()> {
        instructions::remove_liquidity::handler(ctx, lp_amount, min_a_out, min_b_out)
    }

    pub fn swap(
        ctx: Context<Swap>,
        direction: SwapDirection,
        amount_in: u64,
        min_amount_out: u64,
    ) -> Result<()> {
        instructions::swap::handler(ctx, direction, amount_in, min_amount_out)
    }
}
