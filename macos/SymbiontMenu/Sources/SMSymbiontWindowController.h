#import <AppKit/AppKit.h>

#import "SMMenuState.h"

@class SMSymbiontWindowController;

@protocol SMSymbiontWindowControllerDelegate <NSObject>

- (void)symbiontWindowController:(SMSymbiontWindowController *)controller
              changedConnection:(SMConnectionState)connection;
- (void)symbiontWindowController:(SMSymbiontWindowController *)controller
              changedUnreadCount:(NSInteger)unreadCount;

@end

@interface SMSymbiontWindowController : NSWindowController

- (instancetype)initWithEndpoint:(NSURL *)endpoint
                         delegate:(id<SMSymbiontWindowControllerDelegate>)delegate;
- (void)start;
- (void)showSymbiontWindow;
- (void)reloadApplication;

@end
