#import "SMAppDelegate.h"

#import "SMMenuState.h"
#import "SMSymbiontWindowController.h"

@interface SMAppDelegate () <SMSymbiontWindowControllerDelegate>

@property(nonatomic, strong) NSURL *endpoint;
@property(nonatomic, strong) NSStatusItem *statusItem;
@property(nonatomic, strong) NSMenu *contextMenu;
@property(nonatomic, strong) SMSymbiontWindowController *windowController;
@property(nonatomic, strong) SMMenuState *presentation;

@end

@implementation SMAppDelegate

static NSString *const SMHasCompletedFirstLaunchKey = @"hasCompletedFirstLaunch";

- (instancetype)init {
    self = [super init];
    if (self) {
        NSString *configuredURL = NSProcessInfo.processInfo.environment[@"SYMBIONT_URL"];
        _endpoint = [NSURL URLWithString:configuredURL ?: @"http://127.0.0.1:4317/"];
        _presentation = [[SMMenuState alloc] init];
    }
    return self;
}

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    [NSApp setMainMenu:[self makeMainMenu]];
    self.statusItem = [NSStatusBar.systemStatusBar statusItemWithLength:NSVariableStatusItemLength];
    self.windowController = [[SMSymbiontWindowController alloc] initWithEndpoint:self.endpoint delegate:self];
    self.contextMenu = [self makeContextMenu];
    [self configureStatusItem];
    [self.windowController start];
    NSUserDefaults *defaults = NSUserDefaults.standardUserDefaults;
    if (![defaults boolForKey:SMHasCompletedFirstLaunchKey]) {
        [defaults setBool:YES forKey:SMHasCompletedFirstLaunchKey];
        dispatch_async(dispatch_get_main_queue(), ^{
            [self.windowController showSymbiontWindow];
        });
    }
}

- (NSMenu *)makeMainMenu {
    NSMenu *mainMenu = [[NSMenu alloc] initWithTitle:@"symbiont-d"];
    NSMenuItem *editItem = [[NSMenuItem alloc] initWithTitle:@"Edit"
                                                      action:nil
                                               keyEquivalent:@""];
    NSMenu *editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    [self addFirstResponderItemWithTitle:@"Undo" action:@selector(undo:) key:@"z" toMenu:editMenu];
    NSMenuItem *redoItem = [self firstResponderItemWithTitle:@"Redo" action:@selector(redo:) key:@"z"];
    redoItem.keyEquivalentModifierMask = NSEventModifierFlagCommand | NSEventModifierFlagShift;
    [editMenu addItem:redoItem];
    [editMenu addItem:NSMenuItem.separatorItem];
    [self addFirstResponderItemWithTitle:@"Cut" action:@selector(cut:) key:@"x" toMenu:editMenu];
    [self addFirstResponderItemWithTitle:@"Copy" action:@selector(copy:) key:@"c" toMenu:editMenu];
    [self addFirstResponderItemWithTitle:@"Paste" action:@selector(paste:) key:@"v" toMenu:editMenu];
    [self addFirstResponderItemWithTitle:@"Select All" action:@selector(selectAll:) key:@"a" toMenu:editMenu];
    editItem.submenu = editMenu;
    [mainMenu addItem:editItem];
    return mainMenu;
}

- (void)addFirstResponderItemWithTitle:(NSString *)title
                                action:(SEL)action
                                   key:(NSString *)key
                                toMenu:(NSMenu *)menu {
    [menu addItem:[self firstResponderItemWithTitle:title action:action key:key]];
}

- (NSMenuItem *)firstResponderItemWithTitle:(NSString *)title
                                     action:(SEL)action
                                        key:(NSString *)key {
    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:title action:action keyEquivalent:key];
    item.target = nil;
    return item;
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)sender {
    (void)sender;
    return NO;
}

- (BOOL)applicationShouldHandleReopen:(NSApplication *)sender hasVisibleWindows:(BOOL)flag {
    (void)sender;
    if (!flag) {
        [self.windowController showSymbiontWindow];
    }
    return YES;
}

- (void)configureStatusItem {
    NSStatusBarButton *button = self.statusItem.button;
    button.target = self;
    button.action = @selector(statusItemClicked:);
    [button sendActionOn:NSEventMaskLeftMouseUp | NSEventMaskRightMouseUp];
    [self renderStatusItem];
}

- (void)renderStatusItem {
    NSStatusBarButton *button = self.statusItem.button;
    NSString *toolTip = self.presentation.toolTip;
    NSImage *image = [self menuBarImageWithAccessibilityDescription:toolTip];
    image.template = YES;
    button.image = image;
    button.imagePosition = NSImageLeading;
    button.title = self.presentation.countLabel;
    button.toolTip = toolTip;
    button.accessibilityLabel = toolTip;
}

- (NSImage *)menuBarImageWithAccessibilityDescription:(NSString *)description {
    if (self.presentation.connection == SMConnectionStateDisconnected) {
        return [NSImage imageWithSystemSymbolName:self.presentation.symbolName
                         accessibilityDescription:description];
    }

    NSURL *URL = [NSBundle.mainBundle URLForResource:@"MenuBarIcon@2x" withExtension:@"png"];
    NSImage *image = URL == nil ? nil : [[NSImage alloc] initWithContentsOfURL:URL];
    if (image == nil) {
        return [NSImage imageWithSystemSymbolName:self.presentation.symbolName
                         accessibilityDescription:description];
    }
    image.size = NSMakeSize(18, 18);
    image.accessibilityDescription = description;
    return image;
}

- (NSMenu *)makeContextMenu {
    NSMenu *menu = [[NSMenu alloc] init];
    [self addMenuItemWithTitle:@"Open symbiont-d" action:@selector(openWindow:) key:@"" toMenu:menu];
    [self addMenuItemWithTitle:@"Reload" action:@selector(reload:) key:@"r" toMenu:menu];
    [self addMenuItemWithTitle:@"Open in Browser" action:@selector(openInBrowser:) key:@"" toMenu:menu];
    [menu addItem:NSMenuItem.separatorItem];
    [self addMenuItemWithTitle:@"Quit" action:@selector(quit:) key:@"q" toMenu:menu];
    return menu;
}

- (void)addMenuItemWithTitle:(NSString *)title
                      action:(SEL)action
                         key:(NSString *)key
                      toMenu:(NSMenu *)menu {
    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:title action:action keyEquivalent:key];
    item.target = self;
    [menu addItem:item];
}

- (void)symbiontWindowController:(SMSymbiontWindowController *)controller
              changedConnection:(SMConnectionState)connection {
    (void)controller;
    self.presentation.connection = connection;
    [self renderStatusItem];
}

- (void)symbiontWindowController:(SMSymbiontWindowController *)controller
              changedUnreadCount:(NSInteger)unreadCount {
    (void)controller;
    [self.presentation setClampedUnreadCount:unreadCount];
    [self renderStatusItem];
}

- (void)statusItemClicked:(id)sender {
    (void)sender;
    if (NSApp.currentEvent.type == NSEventTypeRightMouseUp) {
        self.statusItem.menu = self.contextMenu;
        [self.statusItem.button performClick:nil];
        self.statusItem.menu = nil;
    } else {
        [self openWindow:nil];
    }
}

- (void)openWindow:(id)sender {
    (void)sender;
    [self.windowController showSymbiontWindow];
}

- (void)reload:(id)sender {
    (void)sender;
    [self.windowController reloadApplication];
    [self.windowController showSymbiontWindow];
}

- (void)openInBrowser:(id)sender {
    (void)sender;
    [NSWorkspace.sharedWorkspace openURL:self.endpoint];
}

- (void)quit:(id)sender {
    (void)sender;
    [NSApp terminate:nil];
}

@end
