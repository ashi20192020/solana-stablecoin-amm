use anchor_lang::prelude::*;

pub mod constants;
pub mod math;

declare_id!("F8JkSibw1r9bfqj1XGaomozVbTq5YPg7L7zqJWKv7evU");

#[program]
pub mod amm {
    use super::*;

    pub fn health_check(_ctx: Context<HealthCheck>) -> Result<()> {
        Ok(())
    }
}

// Anchor 1.x requires every Accounts struct to bind 'info, so a signer stands in
// for what would otherwise be an account-free instruction.
#[derive(Accounts)]
pub struct HealthCheck<'info> {
    pub signer: Signer<'info>,
}
