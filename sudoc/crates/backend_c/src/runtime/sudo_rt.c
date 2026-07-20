#include "sudo_rt.h"

jmp_buf sudo_trap_jmp;
sudo_status sudo_trap_status = SUDO_OK;
int64_t sudo_trap_line = 0;

/* Intrusive header prepended to every sudo allocation, forming a doubly
 * linked list of live blocks so a trap can free everything in O(live). */
typedef struct sudo_alloc_hdr {
    struct sudo_alloc_hdr *prev;
    struct sudo_alloc_hdr *next;
} sudo_alloc_hdr;

static sudo_alloc_hdr sudo_live = {&sudo_live, &sudo_live};

static void sudo_link(sudo_alloc_hdr *h) {
    h->next = sudo_live.next;
    h->prev = &sudo_live;
    sudo_live.next->prev = h;
    sudo_live.next = h;
}

static void sudo_unlink(sudo_alloc_hdr *h) {
    h->prev->next = h->next;
    h->next->prev = h->prev;
}

static void sudo_free_all_live(void) {
    sudo_alloc_hdr *h = sudo_live.next;
    while (h != &sudo_live) {
        sudo_alloc_hdr *next = h->next;
        free(h);
        h = next;
    }
    sudo_live.next = sudo_live.prev = &sudo_live;
}

_Noreturn void sudo_trap(sudo_status status, int64_t line) {
    sudo_trap_status = status;
    sudo_trap_line = line;
    sudo_free_all_live();
    longjmp(sudo_trap_jmp, 1);
}

const char *sudo_status_name(sudo_status status) {
    switch (status) {
        case SUDO_OK: return "Ok";
        case SUDO_TRAP_OUT_OF_BOUNDS: return "OutOfBounds";
        case SUDO_TRAP_KEY_MISSING: return "KeyMissing";
        case SUDO_TRAP_DIV_BY_ZERO: return "DivByZero";
        case SUDO_TRAP_OVERFLOW: return "Overflow";
        case SUDO_TRAP_UNWRAP_FAILED: return "UnwrapFailed";
        case SUDO_TRAP_INVALID_CONVERT: return "InvalidConvert";
        case SUDO_TRAP_INVALID_ARG: return "InvalidArg";
        case SUDO_TRAP_ASSERT_FAILED: return "AssertFailed";
        case SUDO_TRAP_STACK_OVERFLOW: return "StackOverflow";
    }
    return "Unknown";
}

void *sudo_alloc(size_t n) {
    sudo_alloc_hdr *h = malloc(sizeof(sudo_alloc_hdr) + (n ? n : 1));
    if (!h) {
        fprintf(stderr, "sudo: out of memory\n");
        abort();
    }
    sudo_link(h);
    return h + 1;
}

void *sudo_realloc(void *p, size_t n) {
    if (!p) return sudo_alloc(n);
    sudo_alloc_hdr *h = (sudo_alloc_hdr *)p - 1;
    sudo_unlink(h);
    sudo_alloc_hdr *q = realloc(h, sizeof(sudo_alloc_hdr) + (n ? n : 1));
    if (!q) {
        fprintf(stderr, "sudo: out of memory\n");
        abort();
    }
    sudo_link(q);
    return q + 1;
}

void sudo_dealloc(void *p) {
    if (!p) return;
    sudo_alloc_hdr *h = (sudo_alloc_hdr *)p - 1;
    sudo_unlink(h);
    free(h);
}

int64_t *sudo_utf8_decode(const char *s, int64_t *out_n) {
    int64_t cap = 8, n = 0;
    int64_t *buf = sudo_alloc((size_t)cap * sizeof(int64_t));
    const unsigned char *p = (const unsigned char *)s;
    while (*p) {
        uint32_t c;
        int len;
        if (p[0] < 0x80) { c = p[0]; len = 1; }
        else if ((p[0] & 0xE0) == 0xC0) { c = p[0] & 0x1Fu; len = 2; }
        else if ((p[0] & 0xF0) == 0xE0) { c = p[0] & 0x0Fu; len = 3; }
        else if ((p[0] & 0xF8) == 0xF0) { c = p[0] & 0x07u; len = 4; }
        else { sudo_trap(SUDO_TRAP_INVALID_CONVERT, 0); }
        for (int i = 1; i < len; i++) {
            if ((p[i] & 0xC0) != 0x80) sudo_trap(SUDO_TRAP_INVALID_CONVERT, 0);
            c = (c << 6) | (p[i] & 0x3Fu);
        }
        if (c > 0x10FFFF || (c >= 0xD800 && c <= 0xDFFF)) sudo_trap(SUDO_TRAP_INVALID_CONVERT, 0);
        if (n == cap) {
            cap *= 2;
            buf = sudo_realloc(buf, (size_t)cap * sizeof(int64_t));
        }
        buf[n++] = (int64_t)c;
        p += len;
    }
    *out_n = n;
    return buf;
}

char *sudo_utf8_encode(const int64_t *xs, int64_t n) {
    size_t cap = (size_t)n * 4 + 1;
    char *out = malloc(cap ? cap : 1);
    if (!out) { fprintf(stderr, "sudo: out of memory\n"); abort(); }
    size_t w = 0;
    for (int64_t i = 0; i < n; i++) {
        uint32_t c = (uint32_t)xs[i];
        if (c < 0x80) out[w++] = (char)c;
        else if (c < 0x800) {
            out[w++] = (char)(0xC0 | (c >> 6));
            out[w++] = (char)(0x80 | (c & 0x3F));
        } else if (c < 0x10000) {
            out[w++] = (char)(0xE0 | (c >> 12));
            out[w++] = (char)(0x80 | ((c >> 6) & 0x3F));
            out[w++] = (char)(0x80 | (c & 0x3F));
        } else {
            out[w++] = (char)(0xF0 | (c >> 18));
            out[w++] = (char)(0x80 | ((c >> 12) & 0x3F));
            out[w++] = (char)(0x80 | ((c >> 6) & 0x3F));
            out[w++] = (char)(0x80 | (c & 0x3F));
        }
    }
    out[w] = 0;
    return out;
}

char sudo_trap_detail[2048];
static size_t sudo_det_len = 0;

void sudo_det_reset(void) {
    sudo_det_len = 0;
    sudo_trap_detail[0] = 0;
}

void sudo_det_str(const char *s) {
    while (*s && sudo_det_len + 1 < sizeof sudo_trap_detail) {
        sudo_trap_detail[sudo_det_len++] = *s++;
    }
    sudo_trap_detail[sudo_det_len] = 0;
}

void sudo_det_i64(int64_t v) {
    char tmp[32];
    snprintf(tmp, sizeof tmp, "%lld", (long long)v);
    sudo_det_str(tmp);
}

void sudo_det_f64(double v) {
    char tmp[64];
    if (isnan(v)) snprintf(tmp, sizeof tmp, "{\"f\": \"NaN\"}");
    else if (isinf(v)) snprintf(tmp, sizeof tmp, "{\"f\": \"%sInf\"}", v < 0 ? "-" : "");
    else if (v == 0.0 && signbit(v)) snprintf(tmp, sizeof tmp, "{\"f\": \"-0.0\"}");
    else snprintf(tmp, sizeof tmp, "{\"f\": \"%.17g\"}", v);
    sudo_det_str(tmp);
}

void sudo_det_bool(bool v) { sudo_det_str(v ? "true" : "false"); }
