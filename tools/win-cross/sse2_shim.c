/* SSE2 intrinsic shim for clang-cl cross-compilation.
 * clang-cl doesn't inline _mm_* intrinsics from xwin CRT headers.
 * Uses 16-byte struct to match MSVC ABI (returned via hidden pointer,
 * same as xwin CRT's __m128i union). No headers needed.
 */
typedef struct { unsigned char b[16]; } __m128i_shim;

__m128i_shim _mm_loadu_si128(void const *__p) {
    __m128i_shim __r;
    __builtin_memcpy(&__r, __p, 16);
    return __r;
}

void _mm_storeu_si128(void *__p, __m128i_shim __b) {
    __builtin_memcpy(__p, &__b, 16);
}

__m128i_shim _mm_set1_epi8(signed char __b) {
    __m128i_shim __r;
    __builtin_memset(&__r, (unsigned char)__b, 16);
    return __r;
}

__m128i_shim _mm_cmpeq_epi8(__m128i_shim __a, __m128i_shim __b) {
    __m128i_shim __r;
    for (int i = 0; i < 16; i++)
        __r.b[i] = (__a.b[i] == __b.b[i]) ? 0xFF : 0x00;
    return __r;
}

int _mm_movemask_epi8(__m128i_shim __a) {
    int mask = 0;
    for (int i = 0; i < 16; i++)
        if (__a.b[i] & 0x80) mask |= (1 << i);
    return mask;
}
