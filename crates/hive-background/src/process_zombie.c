#include <sys/types.h>
#include <sys/sysctl.h>
#include <sys/proc.h>

/* Point-in-time inventory exclusion only, never proof of group death. */
int hive_inventory_zombie(pid_t pid, uid_t expected_uid) {
    if (pid <= 1) {
        return 0;
    }
    int mib[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, pid};
    struct kinfo_proc kp = {0};
    size_t length = sizeof(kp);
    if (sysctl(mib, 4, &kp, &length, NULL, 0) != 0 || length != sizeof(kp)) {
        return 0;
    }
    return kp.kp_proc.p_pid == pid &&
           kp.kp_eproc.e_ucred.cr_uid == expected_uid &&
           kp.kp_proc.p_stat == SZOMB;
}
