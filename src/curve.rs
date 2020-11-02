//! Swap calculations and curve implementations

use crate::{error::SwapError, helpers::to_u128};

/// Encodes all results of swapping from a source token to a destination token
pub struct SwapResult {
    /// New amount of source token
    pub new_source_amount: u128,
    /// New amount of destination token
    pub new_destination_amount: u128,
    /// Amount of destination token swapped
    pub amount_swapped: u128,
}

/// The StableSwap invariant calculator.
pub struct StableSwap {
    /// Amplification coefficient (A)
    pub amp_factor: u128,
}

impl StableSwap {
    /// New StableSwap calculator
    pub fn new(amp_factor_u64: u64) -> Result<StableSwap, SwapError> {
        let amp_factor = to_u128(amp_factor_u64)?;
        Ok(Self { amp_factor })
    }

    /// Compute stable swap invariant (D)
    /// Equation:
    /// A * sum(x_i) * n**n + D = A * D * n**n + D**(n+1) / (n**n * prod(x_i))
    pub fn compute_d(&self, amount_a: u128, amount_b: u128) -> u128 {
        // XXX: Curve uses u256
        // TODO: Handle overflows
        let n_coins: u128 = 2; // n
        let sum_x = amount_a + amount_b; // sum(x_i), a.k.a S
        if sum_x == 0 {
            0
        } else {
            let mut d_prev: u128;
            let mut d = sum_x;
            let leverage = self.amp_factor * n_coins; // A * n

            // Newton's method to approximate D
            for _ in 0..128 {
                let mut d_p = d;
                d_p = d_p * d / (amount_a * n_coins);
                d_p = d_p * d / (amount_b * n_coins);
                d_prev = d;
                d = (leverage * sum_x + d_p * n_coins) * d
                    / ((leverage - 1) * d + (n_coins + 1) * d_p);
                // Equality with the precision of 1
                if d > d_p {
                    if d - d_prev <= 1 {
                        break;
                    }
                } else if d_prev - d <= 1 {
                    break;
                }
            }

            d
        }
    }

    /// Compute swap amount `y` in proportion to `x`
    /// Solve for y:
    /// y**2 + y * (sum' - (A*n**n - 1) * D / (A * n**n)) = D ** (n + 1) / (n ** (2 * n) * prod' * A)
    /// y**2 + b*y = c
    pub fn compute_y(&self, x: u128, d: u128) -> u128 {
        // XXX: Curve uses u256
        // TODO: Handle overflows
        let n_coins = 2;
        let leverage = self.amp_factor * n_coins; // A * n

        // sum' = prod' = x
        // c =  D ** (n + 1) / (n ** (2 * n) * prod' * A)
        let c = d * d * d / (x * n_coins * n_coins * leverage);
        // b = sum' - (A*n**n - 1) * D / (A * n**n)
        let b = x + d / leverage; // d is subtracted on line 82

        // Solve for y by approximating: y**2 + b*y = c
        let mut y_prev: u128;
        let mut y = d;
        for _ in 0..128 {
            y_prev = y;
            y = (y * y + c) / (2 * y + b - d);
            if y > y_prev {
                if y - y_prev <= 1 {
                    break;
                }
            } else if y_prev - y <= 1 {
                break;
            }
        }

        y
    }

    /// Calcuate withdrawal amount when withdrawing only one type of token
    /// Calculation:
    /// 1. Get current D
    /// 2. Solve Eqn against y_i for D - _token_amount
    pub fn compute_withdraw_one(
        &self,
        pool_token_amount: u64,
        pool_token_supply: u64,
        swap_base_amount: u64,  // Same denomination of token to be withdrawn
        swap_quote_amount: u64, // Counter denomination of token to be withdrawn
        fee_numerator: u64,
        fee_denominator: u64,
    ) -> (u64, u64) {
        // XXX: Curve uses u256
        // TODO: Handle overflows
        let n_coins = 2;
        let d_0 = self.compute_d(swap_base_amount, swap_quote_amount);
        let d_1 = d_0 - pool_token_amount * d_0 / pool_token_supply;
        let new_y = self.compute_y(swap_quote_amount, d_1);

        let fee = fee_numerator * n_coins / (4 * (n_coins - 1)); // XXX: Why divide by 4?
        let expected_base_amount = swap_base_amount * d_1 / d_0 - new_y;
        let expected_quote_amount = swap_quote_amount - swap_quote_amount * d_1 / d_0;
        let new_base_amount = swap_base_amount - expected_base_amount * fee / fee_denominator;
        let new_quote_amount = swap_quote_amount - expected_quote_amount * fee / fee_denominator;

        let dy = new_base_amount - self.compute_y(new_quote_amount, d_1);
        let dy_0 = swap_base_amount - new_y;

        (dy, dy_0 - dy)
    }

    /// Compute SwapResult after an exchange
    pub fn swap_to(
        &self,
        source_amount: u128,
        swap_source_amount: u128,
        swap_destination_amount: u128,
        fee_numerator: u128,
        fee_denominator: u128,
    ) -> Option<SwapResult> {
        let y = self.compute_y(
            swap_source_amount + source_amount,
            self.compute_d(swap_source_amount, swap_destination_amount),
        );
        let dy = swap_destination_amount.checked_sub(y)?;
        let dy_fee = dy
            .checked_mul(fee_numerator)?
            .checked_div(fee_denominator)?;

        let amount_swapped = dy - dy_fee;
        let new_destination_amount = swap_destination_amount.checked_sub(amount_swapped)?;
        let new_source_amount = swap_source_amount.checked_add(source_amount)?;

        Some(SwapResult {
            new_source_amount,
            new_destination_amount,
            amount_swapped,
        })
    }
}

/// Conversions for pool tokens, how much to deposit / withdraw, along with
/// proper initialization
pub struct PoolTokenConverter {
    /// Total supply
    pub supply: u128,
    /// Token A amount
    pub token_a: u128,
    /// Token B amount
    pub token_b: u128,
}

impl PoolTokenConverter {
    /// Create a converter based on existing market information
    pub fn new(supply: u128, token_a: u128, token_b: u128) -> Self {
        Self {
            supply,
            token_a,
            token_b,
        }
    }

    /// A tokens for pool tokens
    pub fn token_a_rate(&self, pool_tokens: u128) -> Option<u128> {
        pool_tokens
            .checked_mul(self.token_a)?
            .checked_div(self.supply)
    }

    /// B tokens for pool tokens
    pub fn token_b_rate(&self, pool_tokens: u128) -> Option<u128> {
        pool_tokens
            .checked_mul(self.token_b)?
            .checked_div(self.supply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;
    use sim::{Model, MODEL_FEE_DENOMINATOR, MODEL_FEE_NUMERATOR};

    fn check_pool_token_a_rate(
        token_a: u128,
        token_b: u128,
        deposit: u128,
        supply: u128,
        expected: Option<u128>,
    ) {
        let calculator = PoolTokenConverter::new(supply, token_a, token_b);
        assert_eq!(calculator.token_a_rate(deposit), expected);
        assert_eq!(calculator.supply, supply);
    }

    #[test]
    fn issued_tokens() {
        check_pool_token_a_rate(2, 50, 5, 10, Some(1));
        check_pool_token_a_rate(10, 10, 5, 10, Some(5));
        check_pool_token_a_rate(5, 100, 5, 10, Some(2));
        check_pool_token_a_rate(5, u128::MAX, 5, 10, Some(2));
        check_pool_token_a_rate(u128::MAX, u128::MAX, 5, 10, None);
    }

    fn check_d(model: &Model, amount_a: u128, amount_b: u128) -> u128 {
        let swap = StableSwap {
            amp_factor: model.amp_factor,
        };
        let d = swap.compute_d(amount_a, amount_b);
        assert_eq!(d, model.sim_d());
        d
    }

    fn check_y(model: &Model, x: u128, d: u128) {
        let swap = StableSwap {
            amp_factor: model.amp_factor,
        };
        assert_eq!(swap.compute_y(x, d), model.sim_y(0, 1, x))
    }

    #[test]
    fn test_curve_math() {
        let n_coin = 2;

        let model_no_balance = Model::new(1, vec![0, 0], n_coin);
        check_d(&model_no_balance, 0, 0);

        let amount_a = 1_000_000_000;
        let amount_b = 1_000_000_000;
        let model_a1 = Model::new(1, vec![amount_a, amount_b], n_coin);
        let d = check_d(&model_a1, amount_a, amount_b);
        check_y(&model_a1, 1, d);
        check_y(&model_a1, 1000, d);
        check_y(&model_a1, amount_a, d);

        let model_a100 = Model::new(100, vec![amount_a, amount_b], n_coin);
        let d = check_d(&model_a100, amount_a, amount_b);
        check_y(&model_a100, 1, d);
        check_y(&model_a100, 1000, d);
        check_y(&model_a100, amount_a, d);

        let model_a1000 = Model::new(1000, vec![amount_a, amount_b], n_coin);
        let d = check_d(&model_a1000, amount_a, amount_b);
        check_y(&model_a1000, 1, d);
        check_y(&model_a1000, 1000, d);
        check_y(&model_a1000, amount_a, d);
    }

    #[test]
    fn test_curve_math_with_random_inputs() {
        let mut rng = rand::thread_rng();

        let n_coin = 2;
        let amp_factor: u128 = rng.gen_range(1, 10_000);
        let amount_a: u128 = rng.gen_range(1, 10_000_000_000);
        let amount_b: u128 = rng.gen_range(1, 10_000_000_000);
        println!("testing curve_math_with_random_inputs:");
        println!(
            "amount_a: {}, amount_b: {}, amp_factor: {}",
            amount_a, amount_b, amp_factor
        );

        let model = Model::new(amp_factor, vec![amount_a, amount_b], n_coin);
        let d = check_d(&model, amount_a, amount_b);
        check_y(&model, rng.gen_range(0, amount_a), d);
    }

    fn check_swap(
        amp_factor: u128,
        source_amount: u128,
        swap_source_amount: u128,
        swap_destination_amount: u128,
    ) {
        let n_coin = 2;
        let swap = StableSwap { amp_factor };
        let result = swap
            .swap_to(
                source_amount,
                swap_source_amount,
                swap_destination_amount,
                MODEL_FEE_NUMERATOR,
                MODEL_FEE_DENOMINATOR,
            )
            .unwrap();
        let model = Model::new(
            swap.amp_factor,
            vec![swap_source_amount, swap_destination_amount],
            n_coin,
        );

        assert_eq!(
            result.amount_swapped,
            model.sim_exchange(0, 1, source_amount)
        );
        assert_eq!(result.new_source_amount, swap_source_amount + source_amount);
        assert_eq!(
            result.new_destination_amount,
            swap_destination_amount - result.amount_swapped
        );
    }

    #[test]
    fn test_swap_calculation() {
        let source_amount: u128 = 10_000_000_000;
        let swap_source_amount: u128 = 50_000_000_000;
        let swap_destination_amount: u128 = 50_000_000_000;

        check_swap(
            1,
            source_amount,
            swap_source_amount,
            swap_destination_amount,
        );
        check_swap(
            10,
            source_amount,
            swap_source_amount,
            swap_destination_amount,
        );
        check_swap(
            100,
            source_amount,
            swap_source_amount,
            swap_destination_amount,
        );
        check_swap(
            1000,
            source_amount,
            swap_source_amount,
            swap_destination_amount,
        );
        check_swap(
            10000,
            source_amount,
            swap_source_amount,
            swap_destination_amount,
        );
    }

    #[test]
    fn test_swap_calculation_with_random_inputs() {
        let mut rng = rand::thread_rng();

        let amp_factor: u128 = rng.gen_range(1, 10_000);
        let source_amount: u128 = rng.gen_range(1, 10_000_000_000);
        let swap_source_amount: u128 = rng.gen_range(1, 10_000_000_000);
        let swap_destination_amount: u128 = rng.gen_range(1, 10_000_000_000);
        println!("testing swap_calculation_with_random_inputs:");
        println!(
            "amp_factor: {}, source_amount: {}, swap_source_amount: {}, swap_source_amount: {}",
            amp_factor, source_amount, swap_source_amount, swap_destination_amount
        );

        check_swap(
            amp_factor,
            source_amount,
            swap_source_amount,
            swap_destination_amount,
        );
    }

    fn check_withdraw_one(
        amp_factor: u64,
        pool_token_amount: u64,
        pool_token_supply: u64,
        swap_base_amount: u64,
        swap_quote_amount: u64,
    ) {
        let n_coin = 2;
        let swap = StableSwap { amp_factor };
        let result = swap.compute_withdraw_one(
            pool_token_amount,
            pool_token_supply,
            swap_base_amount,
            swap_quote_amount,
            MODEL_FEE_NUMERATOR,
            MODEL_FEE_DENOMINATOR,
        );
        let model = Model::new_with_pool_tokens(
            amp_factor,
            vec![swap_base_amount, swap_quote_amount],
            n_coin,
            pool_token_supply,
        );
        assert_eq!(
            result.0,
            model.sim_calc_withdraw_one_coin(pool_token_amount, 0)
        );
    }

    // #[test]
    // fn test_compute_withdraw_one() {
    //     let pool_token_amount = 10000;
    //     let pool_token_supply = 200000;
    //     let swap_base_amount = 1000000;
    //     let swap_quote_amount = 1000000;

    //     let amp_factor = 1;
    //     check_withdraw_one(amp_factor, pool_token_amount, pool_token_supply, swap_base_amount, swap_quote_amount)
    // }
}
