#include <stdlib.h>
long long total(const long long *items, long long count) { long long s = 0; for (long long i = 0; i < count; i++) s += items[i]; return s; }
