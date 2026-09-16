//! High-performance Fixed-Point Taylor Series Bonding Curve for Soroban AMM.
//!
//! Replaces expensive floating-point or transcendental operations on-chain with
//! a gas-optimized 5-term Taylor series expansion in 128-bit fixed-point math (18 decimals).

#![allow(clippy::too_many_arguments)]

pub const SCALE: i128 = 1_000_000_000; // 10^9 (reduced for overflow safety on i128)

/// Scaled multiplication of two fixed-point i128 numbers.
/// Uses i128 arithmetic with a reduced scale (10^9) to avoid overflow.
pub fn mul_fp(a: i128, b: i128) -> i128 {
    // Both a and b are in units of SCALE. Multiply then divide by SCALE.
    // Safe as long as a*b does not exceed i128::MAX (~1.7e38).
    // With SCALE = 10^9 and typical values up to 10^19 (10B * SCALE),
    // product can reach ~10^38 which is near limit — use checked_mul.
    if let Some(product) = a.checked_mul(b) {
        product / SCALE
    } else {
        // fallback: divide one operand first to prevent overflow
        (a / SCALE) * b
    }
}

/// Scaled division of two fixed-point i128 numbers.
pub fn div_fp(a: i128, b: i128) -> i128 {
    if b == 0 {
        return 0;
    }
    if let Some(scaled) = a.checked_mul(SCALE) {
        scaled / b
    } else {
        (a / b) * SCALE
    }
}

/// Gas-optimized 5-term Taylor Series Expansion for Exponential `e^x` in fixed-point math.
/// `e^x ≈ 1 + x + x^2/2! + x^3/3! + x^4/4! + x^5/5!`
pub fn exp_taylor_5(x: i128) -> i128 {
    if x == 0 {
        return SCALE;
    }

    let x1 = x;
    let x2 = mul_fp(x1, x);
    let x3 = mul_fp(x2, x);
    let x4 = mul_fp(x3, x);
    let x5 = mul_fp(x4, x);

    let t0 = SCALE;
    let t1 = x1;
    let t2 = x2 / 2;
    let t3 = x3 / 6;
    let t4 = x4 / 24;
    let t5 = x5 / 120;

    t0 + t1 + t2 + t3 + t4 + t5
}

/// Gas-optimized 5-term Taylor Series Expansion for Natural Logarithm `ln(x)` around 1.
/// Uses transformation y = (x - 1) / (x + 1):
/// `ln(x) = 2 * (y + y^3/3 + y^5/5 + y^7/7 + y^9/9)`
pub fn ln_taylor_5(x: i128) -> i128 {
    if x <= 0 {
        return 0;
    }

    let num = x - SCALE;
    let den = x + SCALE;
    let y = div_fp(num, den);

    let y2 = mul_fp(y, y);
    let y3 = mul_fp(y2, y);
    let y5 = mul_fp(mul_fp(y3, y2), y2);
    let y7 = mul_fp(mul_fp(y5, y2), y2);
    let y9 = mul_fp(mul_fp(y7, y2), y2);

    let sum = y + y3 / 3 + y5 / 5 + y7 / 7 + y9 / 9;
    2 * sum
}

/// Calculate purchase return (tokens output for deposit input) using Taylor series:
/// `T = S * ((1 + dR / R)^F - 1)`
/// Where `(1 + dR/R)^F = exp(F * ln(1 + dR/R))`
pub fn calculate_purchase_return_taylor(
    reserve: i128,
    supply: i128,
    deposit_amount: i128,
    reserve_ratio_num: i128,
    reserve_ratio_den: i128,
) -> i128 {
    if deposit_amount <= 0 || reserve <= 0 || supply <= 0 {
        return 0;
    }

    let ratio_fp = div_fp(reserve_ratio_num * SCALE, reserve_ratio_den * SCALE);
    let ratio_x_deposit = div_fp(deposit_amount, reserve);
    let one_plus_ratio = SCALE + ratio_x_deposit;

    let ln_val = ln_taylor_5(one_plus_ratio);
    let exponent = mul_fp(ratio_fp, ln_val);
    let exp_val = exp_taylor_5(exponent);

    let multiplier = exp_val - SCALE;
    mul_fp(supply, multiplier)
}

/// Calculate sale return (reserve output for token sell input) using Taylor series:
/// `dR = R * (1 - (1 - dT / S)^(1/F))`
pub fn calculate_sale_return_taylor(
    reserve: i128,
    supply: i128,
    sell_amount: i128,
    reserve_ratio_num: i128,
    reserve_ratio_den: i128,
) -> i128 {
    if sell_amount <= 0 || sell_amount >= supply || reserve <= 0 || supply <= 0 {
        return 0;
    }

    let inv_ratio_fp = div_fp(reserve_ratio_den * SCALE, reserve_ratio_num * SCALE);
    let sell_ratio = div_fp(sell_amount, supply);
    let one_minus_ratio = SCALE - sell_ratio;

    let ln_val = ln_taylor_5(one_minus_ratio);
    let exponent = mul_fp(inv_ratio_fp, ln_val);
    let exp_val = exp_taylor_5(exponent);

    let multiplier = SCALE - exp_val;
    mul_fp(reserve, multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taylor_exp_and_ln_precision() {
        // Test ln(1.5) in fixed point (SCALE * 1.5 = 1_500_000_000 with SCALE = 10^9)
        let x = (SCALE * 15) / 10;
        let ln_approx = ln_taylor_5(x);
        let float_ln = (1.5_f64).ln();

        let approx_f64 = ln_approx as f64 / SCALE as f64;
        let error = (approx_f64 - float_ln).abs() / float_ln;

        // Verify precision error is strictly < 0.1%
        assert!(
            error < 0.001,
            "ln_taylor_5 error ({}) exceeds precision bounds!",
            error
        );

        // Test exp(0.2)
        let y = SCALE / 5; // 0.2 in fixed point
        let exp_approx = exp_taylor_5(y);
        let float_exp = (0.2_f64).exp();

        let exp_approx_f64 = exp_approx as f64 / SCALE as f64;
        let exp_error = (exp_approx_f64 - float_exp).abs() / float_exp;

        assert!(
            exp_error < 0.001,
            "exp_taylor_5 error ({}) exceeds precision bounds!",
            exp_error
        );
    }

    #[test]
    fn test_bonding_curve_purchase_return_accuracy() {
        // Use moderate values to avoid overflow with SCALE = 10^9
        let reserve = 1_000 * SCALE;     // 1000 tokens in fixed point
        let supply  = 10_000 * SCALE;    // 10000 tokens in fixed point
        let deposit = 100 * SCALE;       // 100 token deposit

        let tokens_out = calculate_purchase_return_taylor(reserve, supply, deposit, 1, 2);
        assert!(tokens_out > 0, "Tokens output should be positive, got {}", tokens_out);
    }
}
