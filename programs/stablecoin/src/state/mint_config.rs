use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct MintConfig {
    pub admin: Pubkey,
    pub pending_admin: Option<Pubkey>,
    pub compliance_authority: Pubkey,
    pub mint: Pubkey,
    pub symbol: [u8; 8],
    pub decimals: u8,
    pub supply_cap: u64,
    pub total_minted: u128,
    pub total_burned: u128,
    pub paused: bool,
    pub bump: u8,
    pub mint_bump: u8,
    pub _reserved: [u8; 64],
}
