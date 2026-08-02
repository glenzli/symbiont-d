#import <AppKit/AppKit.h>

#import "SMAppDelegate.h"

int main(int argc, const char *argv[]) {
    (void)argc;
    (void)argv;
    @autoreleasepool {
        NSApplication *application = NSApplication.sharedApplication;
        SMAppDelegate *delegate = [[SMAppDelegate alloc] init];
        application.delegate = delegate;
        [application run];
    }
    return 0;
}
