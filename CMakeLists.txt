# Root entry point for the C++20 libraries owned by supra_ffi.
#
# Keeping the canonical native tree inside the crate makes `cargo package -p
# supra_ffi` self-contained. The repository build delegates to that same tree so
# Cargo, CTest, and release builds cannot drift onto different sources.

cmake_minimum_required(VERSION 3.24)
project(supra_cpp_workspace LANGUAGES CXX)

include(CTest)
add_subdirectory(crates/supra_ffi/native)
