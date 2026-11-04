#ifndef THESEUS_MACOS_DESKTOP_BRIDGE_H
#define THESEUS_MACOS_DESKTOP_BRIDGE_H
#include <stdint.h>
#include <stddef.h>
/* All functions are exception-contained. All entry calls are broker-main-thread
 * only. The AppKit completion executes on its documented concurrent queue and
 * touches only synchronized Objective-C-owned storage, never Rust memory. */
typedef struct {
    int32_t status; /* 0=pending, 1=exact manager instance, 2=failed, 3=ambiguous */
    int32_t original_pid; /* correlation only; never reconstruct ownership */
    uint8_t terminated;
    uint8_t has_application;
    uint8_t uncertain_pending;
    uint8_t reserved;
    int64_t error_code;
} TheseusMacSnapshot;
int32_t theseus_mac_session_check(uint32_t supplied_security_session, uint32_t supplied_uid);
int32_t theseus_mac_begin(const char *bundle_path, const char *executable_path,
                         const char *bundle_identifier, const char *const *arguments,
                         size_t count, uint32_t supplied_security_session,
                         uint32_t supplied_uid, void **retained_owner);
int32_t theseus_mac_snapshot(void *owner, TheseusMacSnapshot *out);
int32_t theseus_mac_pump_once(void);
/* Returns 0 while a callback/application may still be live. It never closes or
 * terminates an application and only releases the original retained state. */
int32_t theseus_mac_release_settled(void *owner);
#endif
