use anchor_lang::prelude::*;

#[account]
#[derive(InitSpace)]
pub struct MinterRole {
    pub config: Pubkey,
    pub authority: Pubkey,
    pub allowance: u64,
    pub minted: u64,
    pub bump: u8,
    pub _reserved: [u8; 32],
}
