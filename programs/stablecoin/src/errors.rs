use anchor_lang::prelude::*;

#[error_code]
pub enum StablecoinError {
    #[msg("decimals must equal the protocol stablecoin decimals")]
    UnsupportedDecimals,
    #[msg("supply cap must be greater than zero")]
    ZeroSupplyCap,
    #[msg("supply cap exceeds the protocol maximum")]
    SupplyCapTooLarge,
    #[msg("symbol must be non-empty ASCII alphanumeric right-padded with zero bytes")]
    InvalidSymbol,
    #[msg("name must be non-empty and within the maximum length")]
    InvalidName,
    #[msg("uri must be non-empty and within the maximum length")]
    InvalidUri,
    #[msg("arithmetic overflow")]
    MathOverflow,
    #[msg("mint account is not rent exempt")]
    MintNotRentExempt,
    #[msg("signer is not authorized for this action")]
    Unauthorized,
    #[msg("amount must be greater than zero")]
    ZeroAmount,
    #[msg("minter authority must not be the default pubkey")]
    InvalidAuthority,
    #[msg("allowance must be greater than zero")]
    ZeroAllowance,
    #[msg("mint would exceed the minter allowance")]
    AllowanceExceeded,
    #[msg("mint would exceed the configured supply cap")]
    SupplyCapExceeded,
    #[msg("wallet policy does not allow this token account")]
    WalletNotAllowed,
    #[msg("protocol is paused")]
    ProtocolPaused,
    #[msg("total minted minus total burned does not match mint supply")]
    CounterInvariantViolation,
    #[msg("token account does not belong to this mint")]
    TokenAccountMintMismatch,
    #[msg("token account frozen state does not match the stored wallet policy")]
    PolicyStateMismatch,
}
