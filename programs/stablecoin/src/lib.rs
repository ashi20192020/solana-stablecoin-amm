use anchor_lang::prelude::*;

declare_id!("6GuaNWi16p2a1T6jChe2d2cC2SjKDiCoiHN14tciMzE1");

#[program]
pub mod stablecoin {
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
