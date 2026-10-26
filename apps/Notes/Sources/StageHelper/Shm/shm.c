#include <sys/mman.h>
#include "stage_shm.h"

int stage_shm_open(const char *name, int flags, mode_t mode) {
    return shm_open(name, flags, mode);
}
