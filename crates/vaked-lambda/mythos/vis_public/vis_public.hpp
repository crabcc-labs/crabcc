#ifndef VAKED_VIS_PUBLIC_HPP
#define VAKED_VIS_PUBLIC_HPP

// Closed term: config was known at build time and folded to a constant.
// Baked into the static kernel image; zero runtime dispatch.
namespace vaked {
constexpr const char* value = "public";
} // namespace vaked

#endif // VAKED_VIS_PUBLIC_HPP
