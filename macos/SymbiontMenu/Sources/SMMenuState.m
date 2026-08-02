#import "SMMenuState.h"

@implementation SMMenuState

- (instancetype)init {
    self = [super init];
    if (self) {
        _connection = SMConnectionStateConnecting;
        _unreadCount = 0;
    }
    return self;
}

- (void)setClampedUnreadCount:(NSInteger)count {
    self.unreadCount = MAX(0, count);
}

- (NSString *)symbolName {
    switch (self.connection) {
        case SMConnectionStateConnecting:
            return @"ellipsis.message";
        case SMConnectionStateConnected:
            return self.unreadCount > 0 ? @"message.fill" : @"message";
        case SMConnectionStateDisconnected:
            return @"exclamationmark.bubble";
    }
}

- (NSString *)countLabel {
    if (self.unreadCount == 0) {
        return @"";
    }
    return [NSString stringWithFormat:@" %ld", (long)MIN(self.unreadCount, 99)];
}

- (NSString *)toolTip {
    switch (self.connection) {
        case SMConnectionStateConnecting:
            return @"symbiont-d - connecting";
        case SMConnectionStateConnected:
            return self.unreadCount > 0
                ? [NSString stringWithFormat:@"symbiont-d - %ld unread", (long)self.unreadCount]
                : @"symbiont-d";
        case SMConnectionStateDisconnected:
            return @"symbiont-d - service unavailable";
    }
}

@end
