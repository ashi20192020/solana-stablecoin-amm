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
}
