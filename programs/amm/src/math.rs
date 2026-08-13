use stablecoin::constants::MAX_SUPPLY_CAP;

use crate::constants::{FEE_DENOMINATOR, MAX_FEE_BPS, MINIMUM_LIQUIDITY};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathError {
    ZeroAmount,
    InvalidFee,
    InsufficientLiquidity,
    InsufficientInitialLiquidity,
    ZeroOutput,
    InvalidLiquidityState,
    ValueOutOfRange,
    MathOverflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapQuote {
    pub amount_out: u64,
    pub new_reserve_in: u64,
    pub new_reserve_out: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialLiquidityQuote {
    pub lp_to_user: u64,
    pub lp_to_lock: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddLiquidityQuote {
    pub amount_a: u64,
    pub amount_b: u64,
    pub lp_minted: u64,
    pub new_reserve_a: u64,
    pub new_reserve_b: u64,
    pub new_lp_supply: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoveLiquidityQuote {
    pub amount_a: u64,
    pub amount_b: u64,
    pub new_reserve_a: u64,
    pub new_reserve_b: u64,
    pub new_lp_supply: u64,
}

pub fn integer_sqrt(value: u128) -> Result<u128, MathError> {
    if value < 2 {
        return Ok(value);
    }

    // Digit-by-digit restoring square root: every intermediate stays below `value`,
    // so the full u128 domain including u128::MAX is covered without widening.
    let mut bit = 1u128 << 126;
    while bit > value {
        bit >>= 2;
    }

    let mut remainder = value;
    let mut root = 0u128;
    while bit != 0 {
        let candidate = root.checked_add(bit).ok_or(MathError::MathOverflow)?;
        if remainder >= candidate {
            remainder = remainder
                .checked_sub(candidate)
                .ok_or(MathError::MathOverflow)?;
            root = (root >> 1)
                .checked_add(bit)
                .ok_or(MathError::MathOverflow)?;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }

    Ok(root)
}

pub fn ceil_div(numerator: u128, denominator: u128) -> Result<u128, MathError> {
    if denominator == 0 {
        return Err(MathError::ZeroAmount);
    }

    let quotient = numerator / denominator;
    if numerator % denominator == 0 {
        Ok(quotient)
    } else {
        quotient.checked_add(1).ok_or(MathError::MathOverflow)
    }
}

pub fn quote_swap(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    fee_bps: u16,
) -> Result<SwapQuote, MathError> {
    if amount_in == 0 {
        return Err(MathError::ZeroAmount);
    }
    if reserve_in == 0 || reserve_out == 0 {
        return Err(MathError::InsufficientLiquidity);
    }
    if fee_bps > MAX_FEE_BPS {
        return Err(MathError::InvalidFee);
    }

    let amount_in = widen(amount_in)?;
    let reserve_in = widen(reserve_in)?;
    let reserve_out = widen(reserve_out)?;

    let fee_multiplier = FEE_DENOMINATOR
        .checked_sub(u128::from(fee_bps))
        .ok_or(MathError::InvalidFee)?;
    let in_with_fee = amount_in
        .checked_mul(fee_multiplier)
        .ok_or(MathError::MathOverflow)?;
    let numerator = in_with_fee
        .checked_mul(reserve_out)
        .ok_or(MathError::MathOverflow)?;
    let denominator = reserve_in
        .checked_mul(FEE_DENOMINATOR)
        .ok_or(MathError::MathOverflow)?
        .checked_add(in_with_fee)
        .ok_or(MathError::MathOverflow)?;
    let amount_out = numerator
        .checked_div(denominator)
        .ok_or(MathError::MathOverflow)?;

    if amount_out == 0 {
        return Err(MathError::ZeroOutput);
    }
    if amount_out >= reserve_out {
        return Err(MathError::InsufficientLiquidity);
    }

    let new_reserve_in = reserve_in
        .checked_add(amount_in)
        .ok_or(MathError::MathOverflow)?;
    let new_reserve_out = reserve_out
        .checked_sub(amount_out)
        .ok_or(MathError::MathOverflow)?;

    let old_k = reserve_in
        .checked_mul(reserve_out)
        .ok_or(MathError::MathOverflow)?;
    let new_k = new_reserve_in
        .checked_mul(new_reserve_out)
        .ok_or(MathError::MathOverflow)?;
    if new_k < old_k {
        return Err(MathError::InvalidLiquidityState);
    }

    Ok(SwapQuote {
        amount_out: narrow(amount_out)?,
        new_reserve_in: narrow(new_reserve_in)?,
        new_reserve_out: narrow(new_reserve_out)?,
    })
}

pub fn quote_initial_liquidity(
    amount_a: u64,
    amount_b: u64,
) -> Result<InitialLiquidityQuote, MathError> {
    if amount_a == 0 || amount_b == 0 {
        return Err(MathError::ZeroAmount);
    }

    let product = widen(amount_a)?
        .checked_mul(widen(amount_b)?)
        .ok_or(MathError::MathOverflow)?;
    let lp_total = integer_sqrt(product)?;

    let locked = u128::from(MINIMUM_LIQUIDITY);
    if lp_total <= locked {
        return Err(MathError::InsufficientInitialLiquidity);
    }

    Ok(InitialLiquidityQuote {
        lp_to_user: narrow(
            lp_total
                .checked_sub(locked)
                .ok_or(MathError::MathOverflow)?,
        )?,
        lp_to_lock: MINIMUM_LIQUIDITY,
    })
}

pub fn quote_add_liquidity(
    amount_a_desired: u64,
    amount_b_desired: u64,
    reserve_a: u64,
    reserve_b: u64,
    lp_supply: u64,
) -> Result<AddLiquidityQuote, MathError> {
    if amount_a_desired == 0 || amount_b_desired == 0 {
        return Err(MathError::ZeroAmount);
    }
    if reserve_a == 0 || reserve_b == 0 {
        return Err(MathError::InsufficientLiquidity);
    }
    if lp_supply < MINIMUM_LIQUIDITY {
        return Err(MathError::InvalidLiquidityState);
    }

    let a_desired = widen(amount_a_desired)?;
    let b_desired = widen(amount_b_desired)?;
    let reserve_a = widen(reserve_a)?;
    let reserve_b = widen(reserve_b)?;
    let lp_supply = widen(lp_supply)?;

    // Required deposits round up so the pool never receives less than the ratio demands.
    let b_optimal = ceil_div(
        a_desired
            .checked_mul(reserve_b)
            .ok_or(MathError::MathOverflow)?,
        reserve_a,
    )?;
    let (amount_a, amount_b) = if b_optimal <= b_desired {
        (a_desired, b_optimal)
    } else {
        let a_optimal = ceil_div(
            b_desired
                .checked_mul(reserve_a)
                .ok_or(MathError::MathOverflow)?,
            reserve_b,
        )?;
        if a_optimal > a_desired {
            return Err(MathError::ValueOutOfRange);
        }
        (a_optimal, b_desired)
    };

    let lp_from_a = amount_a
        .checked_mul(lp_supply)
        .ok_or(MathError::MathOverflow)?
        .checked_div(reserve_a)
        .ok_or(MathError::MathOverflow)?;
    let lp_from_b = amount_b
        .checked_mul(lp_supply)
        .ok_or(MathError::MathOverflow)?
        .checked_div(reserve_b)
        .ok_or(MathError::MathOverflow)?;
    let lp_minted = lp_from_a.min(lp_from_b);
    if lp_minted == 0 {
        return Err(MathError::ZeroOutput);
    }

    Ok(AddLiquidityQuote {
        amount_a: narrow(amount_a)?,
        amount_b: narrow(amount_b)?,
        lp_minted: narrow(lp_minted)?,
        new_reserve_a: narrow(
            reserve_a
                .checked_add(amount_a)
                .ok_or(MathError::MathOverflow)?,
        )?,
        new_reserve_b: narrow(
            reserve_b
                .checked_add(amount_b)
                .ok_or(MathError::MathOverflow)?,
        )?,
        new_lp_supply: narrow(
            lp_supply
                .checked_add(lp_minted)
                .ok_or(MathError::MathOverflow)?,
        )?,
    })
}

pub fn quote_remove_liquidity(
    lp_amount: u64,
    reserve_a: u64,
    reserve_b: u64,
    lp_supply: u64,
) -> Result<RemoveLiquidityQuote, MathError> {
    if lp_amount == 0 {
        return Err(MathError::ZeroAmount);
    }
    if reserve_a == 0 || reserve_b == 0 {
        return Err(MathError::InsufficientLiquidity);
    }
    if lp_supply < MINIMUM_LIQUIDITY {
        return Err(MathError::InvalidLiquidityState);
    }
    if lp_amount > lp_supply {
        return Err(MathError::InsufficientLiquidity);
    }

    let lp_amount = widen(lp_amount)?;
    let reserve_a = widen(reserve_a)?;
    let reserve_b = widen(reserve_b)?;
    let lp_supply = widen(lp_supply)?;

    let remaining_lp = lp_supply
        .checked_sub(lp_amount)
        .ok_or(MathError::MathOverflow)?;
    if remaining_lp < u128::from(MINIMUM_LIQUIDITY) {
        return Err(MathError::InsufficientLiquidity);
    }

    // Withdrawals round down, leaving any remainder with the pool.
    let amount_a = lp_amount
        .checked_mul(reserve_a)
        .ok_or(MathError::MathOverflow)?
        .checked_div(lp_supply)
        .ok_or(MathError::MathOverflow)?;
    let amount_b = lp_amount
        .checked_mul(reserve_b)
        .ok_or(MathError::MathOverflow)?
        .checked_div(lp_supply)
        .ok_or(MathError::MathOverflow)?;
    if amount_a == 0 || amount_b == 0 {
        return Err(MathError::ZeroOutput);
    }

    Ok(RemoveLiquidityQuote {
        amount_a: narrow(amount_a)?,
        amount_b: narrow(amount_b)?,
        new_reserve_a: narrow(
            reserve_a
                .checked_sub(amount_a)
                .ok_or(MathError::MathOverflow)?,
        )?,
        new_reserve_b: narrow(
            reserve_b
                .checked_sub(amount_b)
                .ok_or(MathError::MathOverflow)?,
        )?,
        new_lp_supply: narrow(remaining_lp)?,
    })
}

fn widen(value: u64) -> Result<u128, MathError> {
    if value > MAX_SUPPLY_CAP {
        return Err(MathError::ValueOutOfRange);
    }
    Ok(u128::from(value))
}

fn narrow(value: u128) -> Result<u64, MathError> {
    let value = u64::try_from(value).map_err(|_| MathError::ValueOutOfRange)?;
    if value > MAX_SUPPLY_CAP {
        return Err(MathError::ValueOutOfRange);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MAX_SIDE: u64 = MAX_SUPPLY_CAP / 2;
    const MIN_SIDE: u64 = 10_000;

    fn pool_lp_supply(reserve_a: u64, reserve_b: u64) -> u64 {
        let quote = quote_initial_liquidity(reserve_a, reserve_b)
            .expect("generated reserves must form a valid initial pool");
        quote.lp_to_user + quote.lp_to_lock
    }

    prop_compose! {
        fn pool()(
            reserve_a in MIN_SIDE..=MAX_SIDE,
            reserve_b in MIN_SIDE..=MAX_SIDE,
        ) -> (u64, u64, u64) {
            (reserve_a, reserve_b, pool_lp_supply(reserve_a, reserve_b))
        }
    }

    #[test]
    fn integer_sqrt_examples() {
        for (value, expected) in [
            (0u128, 0u128),
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 2),
            (15, 3),
            (16, 4),
            (u128::from(u64::MAX), 4_294_967_295),
        ] {
            assert_eq!(
                integer_sqrt(value).expect("sqrt must succeed"),
                expected,
                "sqrt({value})"
            );
        }
    }

    #[test]
    fn integer_sqrt_covers_the_u128_maximum() {
        assert_eq!(
            integer_sqrt(u128::MAX).expect("sqrt must succeed"),
            u128::from(u64::MAX)
        );

        let largest_square = u128::from(u64::MAX)
            .checked_mul(u128::from(u64::MAX))
            .expect("square of u64::MAX fits in u128");
        assert_eq!(
            integer_sqrt(largest_square).expect("sqrt must succeed"),
            u128::from(u64::MAX)
        );
    }

    #[test]
    fn ceil_div_examples() {
        assert_eq!(ceil_div(10, 5).expect("exact division"), 2);
        assert_eq!(ceil_div(11, 5).expect("rounded division"), 3);
        assert_eq!(ceil_div(0, 5).expect("zero numerator"), 0);
        assert_eq!(ceil_div(u128::MAX, u128::MAX).expect("maximum operands"), 1);
        assert_eq!(ceil_div(1, 0), Err(MathError::ZeroAmount));
    }

    #[test]
    fn swap_quote_example() {
        let quote = quote_swap(1_000, 1_000_000, 1_000_000, 30).expect("swap must succeed");
        assert_eq!(
            quote,
            SwapQuote {
                amount_out: 996,
                new_reserve_in: 1_001_000,
                new_reserve_out: 999_004,
            }
        );
    }

    #[test]
    fn zero_fee_swap_charges_nothing() {
        let quote = quote_swap(1_000, 1_000_000, 1_000_000, 0).expect("swap must succeed");
        assert_eq!(quote.amount_out, 999);
    }

    #[test]
    fn swap_boundary_errors() {
        assert_eq!(
            quote_swap(0, 1_000_000, 1_000_000, 30),
            Err(MathError::ZeroAmount)
        );
        assert_eq!(
            quote_swap(1_000, 0, 1_000_000, 30),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_swap(1_000, 1_000_000, 0, 30),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_swap(1_000, 1_000_000, 1_000_000, MAX_FEE_BPS + 1),
            Err(MathError::InvalidFee)
        );
        assert_eq!(
            quote_swap(MAX_SUPPLY_CAP + 1, 1_000_000, 1_000_000, 30),
            Err(MathError::ValueOutOfRange)
        );
        assert_eq!(
            quote_swap(1, 1_000_000_000, 1_000, 30),
            Err(MathError::ZeroOutput)
        );
        assert_eq!(
            quote_swap(MAX_SUPPLY_CAP, MAX_SUPPLY_CAP, 1_000_000, 30),
            Err(MathError::ValueOutOfRange)
        );
    }

    #[test]
    fn swap_accepts_the_maximum_fee() {
        let quote =
            quote_swap(1_000, 1_000_000, 1_000_000, MAX_FEE_BPS).expect("swap must succeed");
        assert_eq!(quote.amount_out, 989);
    }

    #[test]
    fn initial_liquidity_example() {
        let quote = quote_initial_liquidity(1_000_000, 4_000_000).expect("initial must succeed");
        assert_eq!(
            quote,
            InitialLiquidityQuote {
                lp_to_user: 2_000_000 - MINIMUM_LIQUIDITY,
                lp_to_lock: MINIMUM_LIQUIDITY,
            }
        );
    }

    #[test]
    fn initial_liquidity_boundary_errors() {
        assert_eq!(
            quote_initial_liquidity(0, 1_000_000),
            Err(MathError::ZeroAmount)
        );
        assert_eq!(
            quote_initial_liquidity(1_000_000, 0),
            Err(MathError::ZeroAmount)
        );
        assert_eq!(
            quote_initial_liquidity(MINIMUM_LIQUIDITY, MINIMUM_LIQUIDITY),
            Err(MathError::InsufficientInitialLiquidity)
        );
        assert_eq!(
            quote_initial_liquidity(MAX_SUPPLY_CAP + 1, 1),
            Err(MathError::ValueOutOfRange)
        );

        let smallest = quote_initial_liquidity(MINIMUM_LIQUIDITY + 1, MINIMUM_LIQUIDITY + 1)
            .expect("one unit above the lock must succeed");
        assert_eq!(smallest.lp_to_user, 1);
    }

    #[test]
    fn initial_liquidity_at_the_supply_cap() {
        let quote =
            quote_initial_liquidity(MAX_SUPPLY_CAP, MAX_SUPPLY_CAP).expect("cap must succeed");
        assert_eq!(quote.lp_to_user, MAX_SUPPLY_CAP - MINIMUM_LIQUIDITY);
    }

    #[test]
    fn add_liquidity_uses_the_b_optimal_branch() {
        let quote = quote_add_liquidity(100_000, 250_000, 1_000_000, 2_000_000, 1_414_213)
            .expect("add must succeed");
        assert_eq!(quote.amount_a, 100_000);
        assert_eq!(quote.amount_b, 200_000);
        assert_eq!(quote.lp_minted, 141_421);
        assert_eq!(quote.new_reserve_a, 1_100_000);
        assert_eq!(quote.new_reserve_b, 2_200_000);
        assert_eq!(quote.new_lp_supply, 1_555_634);
    }

    #[test]
    fn add_liquidity_uses_the_a_optimal_branch() {
        let quote = quote_add_liquidity(100_000, 150_000, 1_000_000, 2_000_000, 1_414_213)
            .expect("add must succeed");
        assert_eq!(quote.amount_a, 75_000);
        assert_eq!(quote.amount_b, 150_000);
    }

    #[test]
    fn add_liquidity_rounds_the_required_deposit_up() {
        let quote = quote_add_liquidity(1, 1_000, 3, 4, 1_000).expect("uneven ratio must round up");
        assert_eq!(quote.amount_a, 1);
        assert_eq!(quote.amount_b, 2);
    }

    #[test]
    fn add_liquidity_boundary_errors() {
        assert_eq!(
            quote_add_liquidity(0, 1_000, 1_000_000, 1_000_000, 1_000_000),
            Err(MathError::ZeroAmount)
        );
        assert_eq!(
            quote_add_liquidity(1_000, 0, 1_000_000, 1_000_000, 1_000_000),
            Err(MathError::ZeroAmount)
        );
        assert_eq!(
            quote_add_liquidity(1_000, 1_000, 0, 1_000_000, 1_000_000),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_add_liquidity(1_000, 1_000, 1_000_000, 0, 1_000_000),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_add_liquidity(1_000, 1_000, 1_000_000, 1_000_000, MINIMUM_LIQUIDITY - 1),
            Err(MathError::InvalidLiquidityState)
        );
        assert_eq!(
            quote_add_liquidity(1, 1, 1_000_000_000, 1_000_000_000, 1_000),
            Err(MathError::ZeroOutput)
        );
        assert_eq!(
            quote_add_liquidity(MAX_SUPPLY_CAP + 1, 1_000, 1_000_000, 1_000_000, 1_000_000),
            Err(MathError::ValueOutOfRange)
        );
        assert_eq!(
            quote_add_liquidity(
                MAX_SUPPLY_CAP,
                MAX_SUPPLY_CAP,
                MAX_SUPPLY_CAP,
                MAX_SUPPLY_CAP,
                MAX_SUPPLY_CAP
            ),
            Err(MathError::ValueOutOfRange)
        );
    }

    #[test]
    fn remove_liquidity_example() {
        let quote =
            quote_remove_liquidity(500_000, 1_000_000, 2_000_000, 1_000_000).expect("remove");
        assert_eq!(
            quote,
            RemoveLiquidityQuote {
                amount_a: 500_000,
                amount_b: 1_000_000,
                new_reserve_a: 500_000,
                new_reserve_b: 1_000_000,
                new_lp_supply: 500_000,
            }
        );
    }

    #[test]
    fn remove_liquidity_boundary_errors() {
        assert_eq!(
            quote_remove_liquidity(0, 1_000_000, 1_000_000, 1_000_000),
            Err(MathError::ZeroAmount)
        );
        assert_eq!(
            quote_remove_liquidity(1_000, 0, 1_000_000, 1_000_000),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_remove_liquidity(1_000, 1_000_000, 0, 1_000_000),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_remove_liquidity(500, 1_000_000, 1_000_000, MINIMUM_LIQUIDITY - 1),
            Err(MathError::InvalidLiquidityState)
        );
        assert_eq!(
            quote_remove_liquidity(1_000_001, 1_000_000, 1_000_000, 1_000_000),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_remove_liquidity(999_500, 1_000_000, 1_000_000, 1_000_000),
            Err(MathError::InsufficientLiquidity)
        );
        assert_eq!(
            quote_remove_liquidity(1, 1_000, 1_000, 1_000_000),
            Err(MathError::ZeroOutput)
        );
    }

    #[test]
    fn removing_everything_removable_leaves_the_locked_minimum() {
        let quote = quote_remove_liquidity(
            1_000_000 - MINIMUM_LIQUIDITY,
            1_000_000,
            2_000_000,
            1_000_000,
        )
        .expect("remove must succeed");
        assert_eq!(quote.new_lp_supply, MINIMUM_LIQUIDITY);
    }

    #[test]
    fn supply_cap_bounds_keep_every_intermediate_inside_u128() {
        let cap = u128::from(MAX_SUPPLY_CAP);

        let swap_numerator = cap
            .checked_mul(FEE_DENOMINATOR)
            .and_then(|scaled| scaled.checked_mul(cap));
        assert!(
            swap_numerator.is_some(),
            "swap numerator overflows u128 at MAX_SUPPLY_CAP"
        );

        let post_swap_reserve = cap.checked_mul(2).expect("doubled cap fits in u128");
        assert!(
            post_swap_reserve.checked_mul(post_swap_reserve).is_some(),
            "constant-product check overflows u128 at MAX_SUPPLY_CAP"
        );

        assert!(
            cap.checked_mul(cap).is_some(),
            "liquidity products overflow u128 at MAX_SUPPLY_CAP"
        );
    }

    proptest! {
        #[test]
        fn integer_sqrt_is_the_floor_root(value in any::<u128>()) {
            let root = integer_sqrt(value).expect("sqrt must succeed");
            prop_assert!(root.checked_mul(root).is_some_and(|square| square <= value));

            let next = root.checked_add(1).expect("floor root stays below u128::MAX");
            prop_assert!(next.checked_mul(next).is_none_or(|square| square > value));
        }

        #[test]
        fn ceil_div_is_exact_and_never_rounds_down(
            numerator in any::<u128>(),
            denominator in 1u128..=u128::MAX,
        ) {
            let result = ceil_div(numerator, denominator).expect("ceil_div must succeed");
            let floor = numerator / denominator;

            if numerator % denominator == 0 {
                prop_assert_eq!(result, floor);
            } else {
                prop_assert_eq!(result, floor + 1);
                prop_assert!(result > floor);
            }
            prop_assert!(
                result
                    .checked_mul(denominator)
                    .is_none_or(|product| product >= numerator)
            );
        }

        #[test]
        fn swap_output_stays_below_the_output_reserve(
            amount_in in 1u64..=MAX_SIDE,
            reserve_in in MIN_SIDE..=MAX_SIDE,
            reserve_out in MIN_SIDE..=MAX_SIDE,
            fee_bps in 0u16..=MAX_FEE_BPS,
        ) {
            if let Ok(quote) = quote_swap(amount_in, reserve_in, reserve_out, fee_bps) {
                prop_assert!(quote.amount_out < reserve_out);
                prop_assert_eq!(quote.new_reserve_out, reserve_out - quote.amount_out);
                prop_assert_eq!(quote.new_reserve_in, reserve_in + amount_in);
            }
        }

        #[test]
        fn swap_never_decreases_the_constant_product(
            amount_in in 1u64..=MAX_SIDE,
            reserve_in in MIN_SIDE..=MAX_SIDE,
            reserve_out in MIN_SIDE..=MAX_SIDE,
            fee_bps in 0u16..=MAX_FEE_BPS,
        ) {
            if let Ok(quote) = quote_swap(amount_in, reserve_in, reserve_out, fee_bps) {
                let old_k = u128::from(reserve_in) * u128::from(reserve_out);
                let new_k =
                    u128::from(quote.new_reserve_in) * u128::from(quote.new_reserve_out);
                prop_assert!(new_k >= old_k);
            }
        }

        #[test]
        fn larger_swaps_never_get_a_better_price(
            smaller in 1u64..=MAX_SIDE / 2,
            extra in 1u64..=MAX_SIDE / 2,
            reserve_in in MIN_SIDE..=MAX_SIDE,
            reserve_out in MIN_SIDE..=MAX_SIDE,
            fee_bps in 0u16..=MAX_FEE_BPS,
        ) {
            let larger = smaller + extra;
            let small = quote_swap(smaller, reserve_in, reserve_out, fee_bps);
            let large = quote_swap(larger, reserve_in, reserve_out, fee_bps);

            if let (Ok(small), Ok(large)) = (small, large) {
                let small_price = u128::from(small.amount_out) * u128::from(larger);
                let large_price = u128::from(large.amount_out) * u128::from(smaller);
                prop_assert!(small_price >= large_price);
            }
        }

        #[test]
        fn round_trip_swaps_never_return_more_than_the_input(
            amount_in in 1u64..=MAX_SIDE / 2,
            reserve_in in MIN_SIDE..=MAX_SIDE / 2,
            reserve_out in MIN_SIDE..=MAX_SIDE / 2,
            fee_bps in 0u16..=MAX_FEE_BPS,
        ) {
            let Ok(forward) = quote_swap(amount_in, reserve_in, reserve_out, fee_bps) else {
                return Ok(());
            };
            let back = quote_swap(
                forward.amount_out,
                forward.new_reserve_out,
                forward.new_reserve_in,
                fee_bps,
            );

            if let Ok(back) = back {
                prop_assert!(back.amount_out <= amount_in);
            }
        }

        #[test]
        fn initial_liquidity_locks_exactly_the_minimum(
            amount_a in MIN_SIDE..=MAX_SUPPLY_CAP,
            amount_b in MIN_SIDE..=MAX_SUPPLY_CAP,
        ) {
            let quote = quote_initial_liquidity(amount_a, amount_b)
                .expect("bounded positive amounts must produce a quote");
            prop_assert_eq!(quote.lp_to_lock, MINIMUM_LIQUIDITY);

            let total = u128::from(quote.lp_to_user) + u128::from(quote.lp_to_lock);
            let expected = integer_sqrt(u128::from(amount_a) * u128::from(amount_b))
                .expect("sqrt must succeed");
            prop_assert_eq!(total, expected);
        }

        #[test]
        fn deposits_never_exceed_the_desired_amounts(
            (reserve_a, reserve_b, lp_supply) in pool(),
            amount_a_desired in 1u64..=MAX_SIDE,
            amount_b_desired in 1u64..=MAX_SIDE,
        ) {
            if let Ok(quote) = quote_add_liquidity(
                amount_a_desired,
                amount_b_desired,
                reserve_a,
                reserve_b,
                lp_supply,
            ) {
                prop_assert!(quote.amount_a <= amount_a_desired);
                prop_assert!(quote.amount_b <= amount_b_desired);
            }
        }

        #[test]
        fn minted_lp_never_exceeds_either_proportional_share(
            (reserve_a, reserve_b, lp_supply) in pool(),
            amount_a_desired in 1u64..=MAX_SIDE,
            amount_b_desired in 1u64..=MAX_SIDE,
        ) {
            if let Ok(quote) = quote_add_liquidity(
                amount_a_desired,
                amount_b_desired,
                reserve_a,
                reserve_b,
                lp_supply,
            ) {
                let share_a = u128::from(quote.amount_a) * u128::from(lp_supply)
                    / u128::from(reserve_a);
                let share_b = u128::from(quote.amount_b) * u128::from(lp_supply)
                    / u128::from(reserve_b);
                prop_assert!(u128::from(quote.lp_minted) <= share_a);
                prop_assert!(u128::from(quote.lp_minted) <= share_b);
            }
        }

        #[test]
        fn depositing_then_removing_never_returns_more_than_deposited(
            (reserve_a, reserve_b, lp_supply) in pool(),
            amount_a_desired in 1u64..=MAX_SIDE,
            amount_b_desired in 1u64..=MAX_SIDE,
        ) {
            let Ok(added) = quote_add_liquidity(
                amount_a_desired,
                amount_b_desired,
                reserve_a,
                reserve_b,
                lp_supply,
            ) else {
                return Ok(());
            };

            if let Ok(removed) = quote_remove_liquidity(
                added.lp_minted,
                added.new_reserve_a,
                added.new_reserve_b,
                added.new_lp_supply,
            ) {
                prop_assert!(removed.amount_a <= added.amount_a);
                prop_assert!(removed.amount_b <= added.amount_b);
            }
        }

        #[test]
        fn withdrawals_never_exceed_the_proportional_share(
            (reserve_a, reserve_b, lp_supply) in pool(),
            lp_fraction in 1u64..=10_000,
        ) {
            let lp_amount = (u128::from(lp_supply) * u128::from(lp_fraction) / 10_000)
                .max(1);
            let lp_amount = u64::try_from(lp_amount).expect("fraction of lp_supply fits in u64");

            if let Ok(quote) =
                quote_remove_liquidity(lp_amount, reserve_a, reserve_b, lp_supply)
            {
                prop_assert!(
                    u128::from(quote.amount_a) * u128::from(lp_supply)
                        <= u128::from(lp_amount) * u128::from(reserve_a)
                );
                prop_assert!(
                    u128::from(quote.amount_b) * u128::from(lp_supply)
                        <= u128::from(lp_amount) * u128::from(reserve_b)
                );
                prop_assert_eq!(quote.new_reserve_a, reserve_a - quote.amount_a);
                prop_assert_eq!(quote.new_reserve_b, reserve_b - quote.amount_b);
            }
        }

        #[test]
        fn removing_all_removable_liquidity_leaves_the_minimum(
            (reserve_a, reserve_b, lp_supply) in pool(),
        ) {
            let quote =
                quote_remove_liquidity(lp_supply - MINIMUM_LIQUIDITY, reserve_a, reserve_b, lp_supply)
                    .expect("a full withdrawal must succeed on a generated pool");
            prop_assert_eq!(quote.new_lp_supply, MINIMUM_LIQUIDITY);
        }

        #[test]
        fn capped_inputs_never_overflow(
            amount_in in 1u64..=MAX_SUPPLY_CAP,
            reserve_a in 1u64..=MAX_SUPPLY_CAP,
            reserve_b in 1u64..=MAX_SUPPLY_CAP,
            lp_supply in MINIMUM_LIQUIDITY..=MAX_SUPPLY_CAP,
            fee_bps in 0u16..=MAX_FEE_BPS,
        ) {
            prop_assert_ne!(
                quote_swap(amount_in, reserve_a, reserve_b, fee_bps).err(),
                Some(MathError::MathOverflow)
            );
            prop_assert_ne!(
                quote_initial_liquidity(reserve_a, reserve_b).err(),
                Some(MathError::MathOverflow)
            );
            prop_assert_ne!(
                quote_add_liquidity(amount_in, amount_in, reserve_a, reserve_b, lp_supply).err(),
                Some(MathError::MathOverflow)
            );
            prop_assert_ne!(
                quote_remove_liquidity(lp_supply, reserve_a, reserve_b, lp_supply).err(),
                Some(MathError::MathOverflow)
            );
        }
    }
}
