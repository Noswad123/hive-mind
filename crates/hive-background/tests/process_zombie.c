/* Standalone SDK-typed regression test; no production ABI structs are copied. */
#include <assert.h>
#include <errno.h>
#include <sys/types.h>
#include <sys/sysctl.h>
#include <sys/proc.h>

enum response { VALID, FAILURE, EMPTY, SHORT, OVERSIZED, WRONG_PID, WRONG_UID, LIVE };
static enum response response;
static int calls;

static int fake_sysctl(int *mib, unsigned int count, void *out, size_t *length,
                       void *new_value, size_t new_length) {
    ++calls;
    assert(count == 4 && mib[0] == CTL_KERN && mib[1] == KERN_PROC);
    assert(mib[2] == KERN_PROC_PID && mib[3] == 42);
    assert(new_value == NULL && new_length == 0);
    assert(*length == sizeof(struct kinfo_proc));
    struct kinfo_proc *kp = out;
    kp->kp_proc.p_pid = response == WRONG_PID ? 43 : 42;
    kp->kp_eproc.e_ucred.cr_uid = response == WRONG_UID ? 502 : 501;
    kp->kp_proc.p_stat = response == LIVE ? SRUN : SZOMB;
    switch (response) {
    case FAILURE: errno = ESRCH; return -1;
    case EMPTY: *length = 0; break;
    case SHORT: --*length; break;
    case OVERSIZED: ++*length; break;
    default: break;
    }
    return 0;
}

#define sysctl fake_sysctl
#include "../src/process_zombie.c"
#undef sysctl

int main(void) {
    for (response = VALID; response <= LIVE; ++response) {
        assert(hive_inventory_zombie(42, 501) == (response == VALID));
    }
    int before = calls;
    assert(hive_inventory_zombie(-1, 501) == 0);
    assert(hive_inventory_zombie(0, 501) == 0);
    assert(hive_inventory_zombie(1, 501) == 0);
    assert(calls == before);
    return 0;
}
