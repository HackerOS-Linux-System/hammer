cmake_minimum_required(VERSION 3.18)
project(anvil VERSION 0.0.1 LANGUAGES CXX)

# ─────────────────────────────────────────────────────────────
#  Global settings
# ─────────────────────────────────────────────────────────────

set(CMAKE_CXX_STANDARD          20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS        OFF)

if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

# ─────────────────────────────────────────────────────────────
#  Compiler flags
#
#  Notes:
#   • -Wpedantic removed — it triggers false positives for
#     UTF-8 multi-byte character constants used in rule().
#     We now use raw UTF-8 byte sequences (\xNN) instead, so
#     -Wpedantic would be safe again, but we keep it out to
#     avoid noise from system headers on some distros.
#   • -finput-charset=UTF-8 — tell GCC our source is UTF-8.
#   • -fexec-charset=UTF-8  — output strings are also UTF-8.
#   • -fstack-protector-strong + _FORTIFY_SOURCE — hardening.
# ─────────────────────────────────────────────────────────────

add_compile_options(
    -Wall
    -Wextra
    -finput-charset=UTF-8
    -fexec-charset=UTF-8
    -fstack-protector-strong
    -D_FORTIFY_SOURCE=2
    -fPIC
)

if(CMAKE_BUILD_TYPE STREQUAL "Release")
    add_compile_options(-O2)
elseif(CMAKE_BUILD_TYPE STREQUAL "Debug")
    add_compile_options(-g -O0 -DDEBUG)
endif()

# ─────────────────────────────────────────────────────────────
#  libanvil — static library (linked into hammer via FFI)
# ─────────────────────────────────────────────────────────────

add_library(anvil_static STATIC
    src/anvil_impl.cpp
)

target_include_directories(anvil_static
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:include>
)

set_target_properties(anvil_static PROPERTIES
    OUTPUT_NAME              anvil
    ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib
)

# ─────────────────────────────────────────────────────────────
#  anvil binary — /usr/bin/anvil
# ─────────────────────────────────────────────────────────────

add_executable(anvil_bin
    src/main.cpp
    src/anvil_impl.cpp
)

target_include_directories(anvil_bin PRIVATE include)

set_target_properties(anvil_bin PROPERTIES
    OUTPUT_NAME              anvil
    RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin
)

target_compile_definitions(anvil_bin PRIVATE ANVIL_STANDALONE)

# Threads (needed for std::thread in inotify watch + cmd_watch)
find_package(Threads REQUIRED)
target_link_libraries(anvil_bin PRIVATE Threads::Threads)
target_link_libraries(anvil_static PRIVATE Threads::Threads)

# ─────────────────────────────────────────────────────────────
#  Install
# ─────────────────────────────────────────────────────────────

include(GNUInstallDirs)

install(TARGETS anvil_bin
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}   # /usr/bin/anvil
)

install(TARGETS anvil_static
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}   # /usr/lib/libanvil.a
)

install(FILES include/anvil.h
    DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}/anvil  # /usr/include/anvil/anvil.h
)

# ─────────────────────────────────────────────────────────────
#  Build & install instructions
#
#   cmake -B build -DCMAKE_BUILD_TYPE=Release
#   cmake --build build -j$(nproc)
#   sudo cmake --install build --prefix /usr
#
#  After install:
#   /usr/bin/anvil          ← standalone CLI
#   /usr/lib/libanvil.a     ← static lib for hammer FFI
#   /usr/include/anvil/anvil.h
# ─────────────────────────────────────────────────────────────
