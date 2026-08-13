use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod instructions;
pub mod state;
pub mod token2022;

pub use instructions::*;

declare_id!("6GuaNWi16p2a1T6jChe2d2cC2SjKDiCoiHN14tciMzE1");

#[program]
pub mod stablecoin {
    use super::*;

    pub fn health_check(_ctx: Context<HealthCheck>) -> Result<()> {
        Ok(())
    }

    pub fn initialize_stablecoin(
        ctx: Context<InitializeStablecoin>,
        symbol: [u8; 8],
        name: String,
        uri: String,
        decimals: u8,
        supply_cap: u64,
    ) -> Result<()> {
        instructions::initialize_stablecoin::handler(ctx, symbol, name, uri, decimals, supply_cap)
    }
}

// Anchor 1.x requires every Accounts struct to bind 'info, so a signer stands in
// for what would otherwise be an account-free instruction.
#[derive(Accounts)]
pub struct HealthCheck<'info> {
    pub signer: Signer<'info>,
}
