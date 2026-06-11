#ifndef VAKED_VIS_FROM_ENV_HPP
#define VAKED_VIS_FROM_ENV_HPP

#include <string_view>

namespace vaked {

// Integration seam — wire to MyThOS boot config.
// MyThOS has no env vars; the integrator implements this against
// whatever the kernel exposes (boot args, a compiled-in config blob, …).
// `get_or` returns the value for `key`, or `fallback` if absent.
struct BootConfig {
    const char* get_or(const char* key, const char* fallback) const;
};

const char* vis_from_env(const BootConfig& cfg);

} // namespace vaked

#endif // VAKED_VIS_FROM_ENV_HPP
