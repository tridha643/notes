#include <sys/types.h>
// shm_open is variadic, which Swift can't call; this passes the mode through.
int stage_shm_open(const char *name, int flags, mode_t mode);
