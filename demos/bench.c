/*
 * bench.c  --  CPU-intensive benchmark: Sieve, MatMul 64×64, Pi²/6 series
 *
 * Two tasks chosen to stress different execution patterns:
 *   1. Sieve of Eratosthenes up to 50 000 000  (integer + memory bandwidth)
 *   2. Matrix multiply 256×256 i32             (integer ALU, nested loops)
 *   3. Sum 1/k^2  for k=1..100 000 000           (floating-point division)
 *
 * Compile:  gcc -O3 -o demos/bench_c demos/bench.c
 * Run:      ./demos/bench_c
 *
 * Compare against the Aspect version:
 *   cargo run --release -- compile demos/bench.ap -o /tmp/bench.ll -O3
 *   lli-19 /tmp/bench.ll
 */

#include <stdio.h>
#include <string.h>
#include <time.h>
#include <stdint.h>

#define SIEVE_MAX   50000000
#define MAT_N       256
#define PI2_TERMS   100000000

/* ---- helpers ---- */

static long ms_since(clock_t t0)
{
    return (long)((clock() - t0) * 1000L / CLOCKS_PER_SEC);
}

/* ---- Task 1: Sieve of Eratosthenes ---- */
/*
 * Returns the count of primes in [2, SIEVE_MAX].
 * Expected: 78498
 */
static int sieve_count(void)
{
    static unsigned char composite[SIEVE_MAX + 1];
    memset(composite, 0, sizeof(composite));

    for (int p = 2; (long long)p * p <= SIEVE_MAX; p++) {
        if (!composite[p]) {
            for (int j = p * p; j <= SIEVE_MAX; j += p)
                composite[j] = 1;
        }
    }

    int count = 0;
    for (int i = 2; i <= SIEVE_MAX; i++)
        if (!composite[i]) count++;
    return count;
}

/* ---- Task 2: Matrix multiply C = A × B (64×64 i32) ---- */
/*
 * A[i][j] = (i + j) % 256
 * B[i][j] = (i * j + 1) % 256
 * Returns C[0][0].
 *
 * C[0][0] = Σ_{k=0}^{255} A[0][k] · B[k][0]
 *         = Σ_{k=0}^{255} k · 1
 *         = 255·256/2 = 32640
 */
static int matmul_c00(void)
{
    static int A[MAT_N * MAT_N];
    static int B[MAT_N * MAT_N];
    static int C[MAT_N * MAT_N];

    for (int i = 0; i < MAT_N; i++) {
        for (int j = 0; j < MAT_N; j++) {
            A[i * MAT_N + j] = (i + j) % 256;
            B[i * MAT_N + j] = (i * j + 1) % 256;
            C[i * MAT_N + j] = 0;
        }
    }

    /* ikj loop order: better cache behaviour (A row stays hot) */
    for (int i = 0; i < MAT_N; i++) {
        for (int k = 0; k < MAT_N; k++) {
            int aik = A[i * MAT_N + k];
            for (int j = 0; j < MAT_N; j++)
                C[i * MAT_N + j] += aik * B[k * MAT_N + j];
        }
    }

    return C[0];  /* C[0][0] */
}

/* ---- Task 3: sum 1/k^2 (converges to pi^2/6 ~= 1.6449340668) ---- */
/*
 * Returns the partial sum scaled by 1e10 as int64 so both the C and
 * Aspect versions can print an identical integer for comparison.
 */
static int64_t pi2_over6_scaled(void)
{
    double s = 0.0;
    for (int k = 1; k <= PI2_TERMS; k++) {
        double fk = (double)k;
        s += 1.0 / (fk * fk);
    }
    return (int64_t)(s * 10000000000.0);
}

/* ---- main ---- */

int main(void)
{
    clock_t t0;

    printf("=== CPU Benchmark (C / gcc -O3) ===\n\n");

    t0 = clock();
    int primes = sieve_count();
    printf("Sieve(%d):      primes=%d  [%ldms]\n",
           SIEVE_MAX, primes, ms_since(t0));

    t0 = clock();
    int c00 = matmul_c00();
    printf("MatMul(%dx%d):    C[0][0]=%d  [%ldms]\n",
           MAT_N, MAT_N, c00, ms_since(t0));

    t0 = clock();
    int64_t pi2s = pi2_over6_scaled();
    printf("Pi2/6(%d terms):  1e10*sum=%lld  [%ldms]\n",
           PI2_TERMS, (long long)pi2s, ms_since(t0));

    return 0;
}
