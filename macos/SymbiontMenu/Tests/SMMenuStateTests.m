#import <Foundation/Foundation.h>

#import "SMMenuState.h"

int main(void) {
    @autoreleasepool {
        SMMenuState *state = [[SMMenuState alloc] init];
        state.connection = SMConnectionStateConnected;
        [state setClampedUnreadCount:3];

        NSCAssert([state.symbolName isEqualToString:@"message.fill"], @"Unread should fill the icon");
        NSCAssert([state.countLabel isEqualToString:@" 3"], @"Unread count should be visible");

        [state setClampedUnreadCount:-1];
        NSCAssert(state.unreadCount == 0, @"Unread count must not become negative");

        [state setClampedUnreadCount:120];
        NSCAssert([state.countLabel isEqualToString:@" 99"], @"The visible count should be bounded");

        state.connection = SMConnectionStateDisconnected;
        NSCAssert(
            [state.symbolName isEqualToString:@"exclamationmark.bubble"],
            @"Disconnected state should take visual priority"
        );
    }
    return 0;
}
