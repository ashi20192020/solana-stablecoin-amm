use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PolicyStatus {
    Allowed,
    Blocked,
}

impl PolicyStatus {
    pub fn is_blocked(self) -> bool {
        self == Self::Blocked
    }
}

#[account]
#[derive(InitSpace)]
pub struct WalletPolicy {
    pub mint: Pubkey,
    pub token_account: Pubkey,
    pub owner: Pubkey,
    pub status: PolicyStatus,
    pub updated_at: i64,
    pub updated_by: Pubkey,
    pub bump: u8,
    pub _reserved: [u8; 32],
}
