#include "vis_from_env.hpp"

namespace vaked {

const char* vis_from_env(const BootConfig& cfg) {
    const std::string_view scrutinee{cfg.get_or("MASTODON_VISIBILITY", "unlisted")};
    if (scrutinee == "public") {
        return "public";
    }
    else if (scrutinee == "private") {
        return "private";
    }
    else if (scrutinee == "direct") {
        return "direct";
    }
    return "unlisted";
}

} // namespace vaked
