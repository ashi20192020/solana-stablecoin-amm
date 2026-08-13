use anchor_lang::prelude::*;

pub mod constants;
pub mod errors;
pub mod instructions;
pub mod state;
pub mod token2022;

pub use instructions::*;
use state::PolicyStatus;

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

    pub fn grant_minter(ctx: Context<GrantMinter>, allowance: u64) -> Result<()> {
        instructions::grant_minter::handler(ctx, allowance)
    }

    pub fn revoke_minter(ctx: Context<RevokeMinter>) -> Result<()> {
        instructions::revoke_minter::handler(ctx)
    }

    pub fn set_wallet_policy(ctx: Context<SetWalletPolicy>, status: PolicyStatus) -> Result<()> {
        instructions::set_wallet_policy::handler(ctx, status)
    }

    pub fn mint_to(ctx: Context<MintTo>, amount: u64) -> Result<()> {
        instructions::mint_to::handler(ctx, amount)
    }

    pub fn burn(ctx: Context<Burn>, amount: u64) -> Result<()> {
        instructions::burn::handler(ctx, amount)
    }
}

// Anchor 1.x requires every Accounts struct to bind 'info, so a signer stands in
// for what would otherwise be an account-free instruction.
#[derive(Accounts)]
pub struct HealthCheck<'info> {
    pub signer: Signer<'info>,
}
