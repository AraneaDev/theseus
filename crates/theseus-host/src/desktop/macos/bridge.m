#import <AppKit/AppKit.h>
#import <Security/AuthSession.h>
#import <unistd.h>
#import "bridge.h"

@interface TheseusMacActivation : NSObject
@property(nonatomic, strong) NSWorkspace *workspace;
@property(nonatomic, strong) NSWorkspaceOpenConfiguration *configuration;
@property(nonatomic, strong) NSURL *bundleURL;
@property(nonatomic, strong) NSURL *executableURL;
@property(nonatomic, copy) NSString *bundleIdentifier;
@property(nonatomic, strong) NSDate *requestedAt;
@property(nonatomic, strong) NSRunningApplication *application;
@property(nonatomic, strong) NSError *error;
@property(nonatomic, strong) NSException *exception;
@property(nonatomic, copy) void (^completion)(NSRunningApplication *, NSError *);
@property(nonatomic) BOOL complete;
@property(nonatomic) BOOL exact;
@property(nonatomic) BOOL uncertainPending;
@end
@implementation TheseusMacActivation
@end

int32_t theseus_mac_session_check(uint32_t supplied, uint32_t uid) {
    @try {
        if (![NSThread isMainThread] || supplied == 0 || uid != geteuid()) return 0;
        SecuritySessionId actual = 0;
        SessionAttributeBits attributes = 0;
        if (SessionGetInfo(callerSecuritySession, &actual, &attributes) != errSecSuccess) return 0;
        return actual == supplied && (attributes & sessionHasGraphicAccess) != 0;
    } @catch (__unused NSException *exception) { return 0; }
}
static NSString *text(const char *value) {
    return value ? [[NSString alloc] initWithUTF8String:value] : nil;
}
int32_t theseus_mac_begin(const char *bundle_path, const char *executable_path,
                         const char *identifier, const char *const *arguments,
                         size_t count, uint32_t session, uint32_t uid, void **out) {
    if (!out) return 0;
    *out = NULL;
    @autoreleasepool {
        @try {
            if (!theseus_mac_session_check(session, uid) || count > 64) return 0;
            NSString *bundle = text(bundle_path), *executable = text(executable_path), *bundleID = text(identifier);
            if (!bundle || !executable || !bundleID || ![bundle isAbsolutePath] || ![executable isAbsolutePath]) return 0;
            NSMutableArray<NSString *> *argv = [NSMutableArray arrayWithCapacity:count];
            for (size_t i = 0; i < count; ++i) {
                NSString *arg = text(arguments[i]);
                if (!arg) return 0;
                [argv addObject:arg];
            }
            TheseusMacActivation *state = [TheseusMacActivation new];
            state.bundleURL = [NSURL fileURLWithPath:bundle isDirectory:YES];
            state.executableURL = [NSURL fileURLWithPath:executable isDirectory:NO];
            state.bundleIdentifier = bundleID;
            state.configuration = [NSWorkspaceOpenConfiguration configuration];
            state.configuration.createsNewApplicationInstance = YES;
            state.configuration.allowsRunningApplicationSubstitution = NO;
            state.configuration.promptsUserIfNeeded = NO;
            state.configuration.addsToRecentItems = NO;
            state.configuration.activates = NO;
            state.configuration.arguments = argv;
            state.configuration.environment = @{};
            state.workspace = [NSWorkspace sharedWorkspace];
            state.requestedAt = [NSDate date];
            // Intentional strong cycle: the original callback/configuration must
            // survive uncertain exceptions and late completion. Only a terminal
            // callback clears it; a weak capture could free unresolved storage.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Warc-retain-cycles"
            state.completion = ^(NSRunningApplication *application, NSError *error) {
                @autoreleasepool {
                    @try {
                        @synchronized (state) {
                            if (state.complete) return;
                            state.application = application;
                            state.error = error;
                            // The exact returned manager object is retained even
                            // when it cannot authenticate the requested instance.
                            state.exact = application && !error
                                && [application.bundleURL.path isEqualToString:state.bundleURL.path]
                                && [application.executableURL.path isEqualToString:state.executableURL.path]
                                && [application.bundleIdentifier isEqualToString:state.bundleIdentifier]
                                && application.launchDate
                                && [application.launchDate compare:state.requestedAt] != NSOrderedAscending;
                            state.complete = YES;
                            state.uncertainPending = NO;
                            state.completion = nil; // break original block cycle
                        }
                    } @catch (NSException *exception) {
                        @synchronized (state) {
                            state.application = application;
                            state.error = error;
                            state.exception = exception;
                            state.exact = NO;
                            state.complete = YES;
                            state.uncertainPending = NO;
                            state.completion = nil;
                        }
                    }
                }
            };
#pragma clang diagnostic pop
            // Publish a retained native owner before any launcher effect. An
            // exception/lost reply cannot destroy callback/configuration state.
            *out = (void *)CFBridgingRetain(state);
            @try {
                [state.workspace openApplicationAtURL:state.bundleURL
                    configuration:state.configuration completionHandler:state.completion];
            } @catch (NSException *exception) {
                @synchronized (state) {
                    state.exception = exception;
                    // No documented terminal callback receipt exists. Preserve
                    // pending storage rather than infer that launch never ran.
                    if (!state.complete) state.uncertainPending = YES;
                }
                return -1;
            }
            return 1;
        } @catch (__unused NSException *exception) {
            // Before retained publication this is pre-effect refusal. After it,
            // caller must retain the original owner even if this returns -1.
            return *out ? -1 : 0;
        }
    }
}
int32_t theseus_mac_snapshot(void *owner, TheseusMacSnapshot *out) {
    if (!owner || !out || ![NSThread isMainThread]) return 0;
    @autoreleasepool {
        @try {
            TheseusMacActivation *state = (__bridge TheseusMacActivation *)owner;
            @synchronized (state) {
                *out = (TheseusMacSnapshot){0};
                out->uncertain_pending = state.uncertainPending;
                out->has_application = state.application != nil;
                out->error_code = state.error.code;
                if (!state.complete) return 1;
                out->status = state.exact ? 1 : state.application ? 3 : state.exception ? 3 : 2;
                if (state.application) {
                    out->original_pid = state.application.processIdentifier;
                    out->terminated = state.application.terminated;
                }
                return 1;
            }
        } @catch (__unused NSException *exception) { return 0; }
    }
}
int32_t theseus_mac_pump_once(void) {
    @autoreleasepool {
        @try {
            if (![NSThread isMainThread]) return 0;
            [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.001]];
            return 1;
        } @catch (__unused NSException *exception) { return 0; }
    }
}
int32_t theseus_mac_release_settled(void *owner) {
    if (!owner || ![NSThread isMainThread]) return 0;
    @autoreleasepool {
        @try {
            TheseusMacActivation *state = (__bridge TheseusMacActivation *)owner;
            @synchronized (state) {
                if (!state.complete || (state.application && !state.application.terminated)) return 0;
            }
            // No PID attachment, task port, terminate or forceTerminate calls.
            CFRelease(owner);
            return 1;
        } @catch (__unused NSException *exception) { return 0; }
    }
}
