#import <Foundation/Foundation.h>

typedef NS_ENUM(NSInteger, SMConnectionState) {
    SMConnectionStateConnecting,
    SMConnectionStateConnected,
    SMConnectionStateDisconnected,
};

@interface SMMenuState : NSObject

@property(nonatomic) SMConnectionState connection;
@property(nonatomic) NSInteger unreadCount;

- (void)setClampedUnreadCount:(NSInteger)count;
- (NSString *)symbolName;
- (NSString *)countLabel;
- (NSString *)toolTip;

@end
