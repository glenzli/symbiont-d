#import "SMSymbiontWindowController.h"

#import <WebKit/WebKit.h>

@interface SMSymbiontWindowController () <NSWindowDelegate, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler>

@property(nonatomic, strong) NSURL *endpoint;
@property(nonatomic, weak) id<SMSymbiontWindowControllerDelegate> presentationDelegate;
@property(nonatomic, strong) WKWebView *webView;
@property(nonatomic, strong) NSTimer *retryTimer;
@property(nonatomic) SMConnectionState connection;

@end

@implementation SMSymbiontWindowController

- (instancetype)initWithEndpoint:(NSURL *)endpoint
                         delegate:(id<SMSymbiontWindowControllerDelegate>)delegate {
    WKWebViewConfiguration *configuration = [[WKWebViewConfiguration alloc] init];
    WKWebView *webView = [[WKWebView alloc] initWithFrame:NSZeroRect configuration:configuration];
    NSWindow *window = [[NSWindow alloc]
        initWithContentRect:NSMakeRect(0, 0, 720, 820)
                  styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                            NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
                    backing:NSBackingStoreBuffered
                      defer:NO];
    window.title = @"symbiont-d";
    window.minSize = NSMakeSize(460, 560);
    window.releasedWhenClosed = NO;
    window.contentView = webView;

    self = [super initWithWindow:window];
    if (self) {
        _endpoint = endpoint;
        _presentationDelegate = delegate;
        _webView = webView;
        _connection = SMConnectionStateConnecting;

        window.delegate = self;
        [window setFrameAutosaveName:@"symbiont-d-main-window"];
        webView.navigationDelegate = self;
        webView.UIDelegate = self;
        [configuration.userContentController addScriptMessageHandler:self name:@"symbiontNative"];
    }
    return self;
}

- (void)dealloc {
    [self.retryTimer invalidate];
    [self.webView.configuration.userContentController removeScriptMessageHandlerForName:@"symbiontNative"];
}

- (void)start {
    [self loadApplication];
}

- (void)showSymbiontWindow {
    [super showWindow:nil];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
}

- (void)reloadApplication {
    [self loadApplication];
}

- (void)loadApplication {
    [self.retryTimer invalidate];
    self.retryTimer = nil;
    [self setConnectionState:SMConnectionStateConnecting];
    NSURLRequest *request = [NSURLRequest requestWithURL:self.endpoint
                                            cachePolicy:NSURLRequestReloadIgnoringLocalCacheData
                                        timeoutInterval:10];
    [self.webView loadRequest:request];
}

- (void)setConnectionState:(SMConnectionState)connection {
    if (self.connection == connection) {
        return;
    }
    self.connection = connection;
    [self.presentationDelegate symbiontWindowController:self changedConnection:connection];
    if (connection == SMConnectionStateDisconnected) {
        [self scheduleRetry];
    } else {
        [self.retryTimer invalidate];
        self.retryTimer = nil;
    }
}

- (void)scheduleRetry {
    if (self.retryTimer != nil) {
        return;
    }
    self.retryTimer = [NSTimer scheduledTimerWithTimeInterval:5
                                                      target:self
                                                    selector:@selector(probeService:)
                                                    userInfo:nil
                                                     repeats:YES];
}

- (void)probeService:(NSTimer *)timer {
    (void)timer;
    NSURL *healthURL = [NSURL URLWithString:@"api/health" relativeToURL:self.endpoint].absoluteURL;
    NSMutableURLRequest *request = [NSMutableURLRequest requestWithURL:healthURL];
    request.cachePolicy = NSURLRequestReloadIgnoringLocalCacheData;
    request.timeoutInterval = 2;
    __weak typeof(self) weakSelf = self;
    [[[NSURLSession sharedSession] dataTaskWithRequest:request
                                    completionHandler:^(__unused NSData *data, NSURLResponse *response, __unused NSError *error) {
        NSHTTPURLResponse *httpResponse = (NSHTTPURLResponse *)response;
        if (![httpResponse isKindOfClass:NSHTTPURLResponse.class] ||
            httpResponse.statusCode < 200 || httpResponse.statusCode >= 300) {
            return;
        }
        dispatch_async(dispatch_get_main_queue(), ^{
            [weakSelf loadApplication];
        });
    }] resume];
}

- (void)showOfflinePage {
    [self setConnectionState:SMConnectionStateDisconnected];
    [self.webView loadHTMLString:[self.class offlineHTML] baseURL:nil];
}

- (BOOL)isApplicationURL:(NSURL *)URL {
    if (URL.host == nil || self.endpoint.host == nil) {
        return NO;
    }
    NSNumber *URLPort = URL.port ?: [self defaultPortForURL:URL];
    NSNumber *endpointPort = self.endpoint.port ?: [self defaultPortForURL:self.endpoint];
    return [URL.scheme isEqualToString:self.endpoint.scheme] &&
           [URL.host isEqualToString:self.endpoint.host] &&
           [URLPort isEqualToNumber:endpointPort];
}

- (NSNumber *)defaultPortForURL:(NSURL *)URL {
    return [URL.scheme isEqualToString:@"https"] ? @443 : @80;
}

+ (NSString *)offlineHTML {
    return @"<!doctype html><html lang='zh-CN'><head>"
            "<meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>"
            "<style>:root{color-scheme:light dark;font-family:-apple-system,BlinkMacSystemFont,sans-serif}"
            "body{margin:0;min-height:100vh;display:grid;place-items:center;background:Canvas;color:CanvasText}"
            "main{width:min(28rem,calc(100% - 3rem))}h1{margin:0 0 .6rem;font-size:1.15rem;letter-spacing:0}"
            "p{margin:0 0 1.25rem;opacity:.62;line-height:1.55}button{border:1px solid color-mix(in srgb,CanvasText 22%,transparent);"
            "border-radius:6px;padding:.55rem .85rem;background:Canvas;color:CanvasText;font:inherit;cursor:pointer}</style>"
            "</head><body><main><h1>symbiont-d 暂时不可用</h1>"
            "<p>后台服务可能正在启动或重新连接。恢复后，这个窗口会自动重试。</p>"
            "<button onclick=\"window.webkit.messageHandlers.symbiontNative.postMessage({type:'retry'})\">立即重试</button>"
            "</main></body></html>";
}

- (BOOL)windowShouldClose:(NSWindow *)sender {
    [sender orderOut:nil];
    return NO;
}

- (void)webView:(WKWebView *)webView didFinishNavigation:(WKNavigation *)navigation {
    (void)navigation;
    if ([self isApplicationURL:webView.URL]) {
        [self setConnectionState:SMConnectionStateConnected];
    }
}

- (void)webView:(WKWebView *)webView
        didFailProvisionalNavigation:(WKNavigation *)navigation
                         withError:(NSError *)error {
    (void)webView;
    (void)navigation;
    (void)error;
    [self showOfflinePage];
}

- (void)webView:(WKWebView *)webView
        didFailNavigation:(WKNavigation *)navigation
                withError:(NSError *)error {
    (void)webView;
    (void)navigation;
    (void)error;
    [self showOfflinePage];
}

- (void)webView:(WKWebView *)webView
        decidePolicyForNavigationAction:(WKNavigationAction *)navigationAction
        decisionHandler:(void (^)(WKNavigationActionPolicy))decisionHandler {
    (void)webView;
    NSURL *URL = navigationAction.request.URL;
    if (navigationAction.navigationType == WKNavigationTypeLinkActivated &&
        URL != nil && ![self isApplicationURL:URL]) {
        [NSWorkspace.sharedWorkspace openURL:URL];
        decisionHandler(WKNavigationActionPolicyCancel);
        return;
    }
    decisionHandler(WKNavigationActionPolicyAllow);
}

- (nullable WKWebView *)webView:(WKWebView *)webView
       createWebViewWithConfiguration:(WKWebViewConfiguration *)configuration
                  forNavigationAction:(WKNavigationAction *)navigationAction
                       windowFeatures:(WKWindowFeatures *)windowFeatures {
    (void)webView;
    (void)configuration;
    (void)windowFeatures;
    if (navigationAction.request.URL != nil) {
        [NSWorkspace.sharedWorkspace openURL:navigationAction.request.URL];
    }
    return nil;
}

- (void)userContentController:(WKUserContentController *)userContentController
      didReceiveScriptMessage:(WKScriptMessage *)message {
    (void)userContentController;
    if (![message.name isEqualToString:@"symbiontNative"] ||
        ![message.body isKindOfClass:NSDictionary.class]) {
        return;
    }
    NSDictionary *payload = (NSDictionary *)message.body;
    NSString *type = payload[@"type"];
    if ([type isEqualToString:@"unread"]) {
        [self.presentationDelegate symbiontWindowController:self
                                         changedUnreadCount:[payload[@"count"] integerValue]];
    } else if ([type isEqualToString:@"connection"]) {
        [self setConnectionState:[payload[@"connected"] boolValue]
            ? SMConnectionStateConnected
            : SMConnectionStateDisconnected];
    } else if ([type isEqualToString:@"retry"]) {
        [self loadApplication];
    }
}

@end
