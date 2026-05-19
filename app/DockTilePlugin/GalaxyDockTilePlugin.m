#include "GalaxyDockTilePlugin.h"

@implementation GalaxyDockTilePlugIn {
    NSFileHandle *_logFileHandle;
}

@synthesize iconChangedObserver;
@synthesize defaultsObserver;

- (void)logMessage:(NSString *)message {
    if (_logFileHandle) {
        NSDateFormatter *formatter = [[NSDateFormatter alloc] init];
        [formatter setDateFormat:@"yyyy-MM-dd HH:mm:ss.SSS"];
        NSString *timestamp = [formatter stringFromDate:[NSDate date]];
        NSString *logEntry = [NSString stringWithFormat:@"[%@] %@\n", timestamp, message];
        [_logFileHandle writeData:[logEntry dataUsingEncoding:NSUTF8StringEncoding]];
        [_logFileHandle synchronizeFile];
    }
}

- (instancetype)init {
    self = [super init];
    if (self) {
        @try {
            NSDateFormatter *formatter = [[NSDateFormatter alloc] init];
            [formatter setDateFormat:@"yyyy-MM-dd_HH-mm-ss"];
            NSString *timestamp = [formatter stringFromDate:[NSDate date]];
            NSString *logPath = [NSString stringWithFormat:@"/tmp/galaxy_docktile_%@.log", timestamp];
            [[NSFileManager defaultManager] createFileAtPath:logPath contents:nil attributes:nil];
            _logFileHandle = [NSFileHandle fileHandleForWritingAtPath:logPath];
            [self logMessage:@"GalaxyDockTilePlugin initialized"];
        } @catch (NSException *exception) {
            NSLog(@"Exception during initialization: %@\nStack trace: %@",
                  exception.reason,
                  exception.callStackSymbols);
        }
    }
    return self;
}

- (void)updateAppIcon:(NSDockTile *)tile {
    @try {
        [self logMessage:@"updateAppIcon called"];
        NSBundle *pluginBundle = [NSBundle bundleForClass:[self class]];
        NSString *path = [[pluginBundle bundlePath] stringByAppendingPathComponent:@"Contents/Info.plist"];
        NSDictionary *dict = [NSDictionary dictionaryWithContentsOfFile:path];
        NSString *bundleId = dict[@"MainAppBundleIdentifier"];
        [self logMessage:[NSString stringWithFormat:@"Plugin Bundle ID: %@", bundleId]];

        NSUserDefaults *hostDefaults = [[NSUserDefaults alloc] initWithSuiteName:bundleId];
        [hostDefaults synchronize];

        NSString* appIconName = [hostDefaults stringForKey:@"AppIcon"];

        NSString* cleanName = [[appIconName stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"\""]] lowercaseString];
        if (!appIconName || [appIconName length] == 0 || [cleanName isEqualToString:@"galaxy"]) {
            [self logMessage:@"User has default icon, resetting dock tile to system default"];
            [tile setContentView:nil];
            [tile display];
            return;
        }

        NSString* iconFileName = [self convertAppIconNameToFileName:appIconName];
        [self logMessage:[NSString stringWithFormat:@"Icon file name: %@", iconFileName]];

        NSImage* currentImage = [self LoadDockTileImage:iconFileName];

        NSImageView* imageView = [[NSImageView alloc] initWithFrame:NSMakeRect(0, 0, 256, 256)];
        [imageView setImage:currentImage];
        [imageView setImageScaling:NSImageScaleProportionallyUpOrDown];
        [tile setContentView:imageView];
        [tile display];

        [self logMessage:[NSString stringWithFormat:@"Dock tile updated with icon: %@", iconFileName]];
    } @catch (NSException *exception) {
        [self logMessage:[NSString stringWithFormat:@"Exception updating dock tile icon: %@\nStack trace: %@\nTile: %@",
              exception.reason,
              exception.callStackSymbols,
              tile ? @"valid" : @"nil"]];
    }
}

- (NSString*)convertAppIconNameToFileName:(NSString*)appIconName {
    NSString* cleanName = [[appIconName stringByTrimmingCharactersInSet:[NSCharacterSet characterSetWithCharactersInString:@"\""]] lowercaseString];

    NSDictionary* mapping = @{
        @"galaxy": @"galaxy",
        @"dotmatrix": @"galaxy_dotmatrix",
        @"explorer": @"galaxy_explorer",
        @"rainbow": @"galaxy_rainbow",
        @"wormhole": @"galaxy_wormhole",
    };

    NSString* fileName = mapping[cleanName];
    return fileName ?: @"galaxy";
}

- (NSImage*)LoadDockTileImage:(NSString*)imageName {
    NSBundle* pluginBundle = [NSBundle bundleForClass:[self class]];
    NSString* imagePath = [pluginBundle pathForResource:imageName ofType:@"png"];
    [self logMessage:[NSString stringWithFormat:@"Image path: %@", imagePath]];
    if (imagePath == nil) {
        [self logMessage:[NSString stringWithFormat:@"Could not find image named %@ in the plugin resources", imageName]];
        return nil;
    }
    return [[NSImage alloc] initWithContentsOfFile:imagePath];
}

- (void)setDockTile:(NSDockTile *)dockTile {
    @try {
        [self logMessage:[NSString stringWithFormat:@"setDockTile called with tile: %@", dockTile ? @"valid" : @"nil"]];
        if (dockTile) {
            NSBundle *pluginBundle = [NSBundle bundleForClass:[GalaxyDockTilePlugIn class]];
            NSString *path = [[pluginBundle bundlePath] stringByAppendingPathComponent:@"Contents/Info.plist"];
            NSDictionary *dict = [NSDictionary dictionaryWithContentsOfFile:path];
            NSString *bundleId = dict[@"MainAppBundleIdentifier"];

            [self logMessage:[NSString stringWithFormat:@"Main app bundleId: %@", bundleId]];

            NSUserDefaults *hostDefaults = [[NSUserDefaults alloc] initWithSuiteName:bundleId];
            [hostDefaults addObserver:self
                        forKeyPath:@"AppIcon"
                            options:NSKeyValueObservingOptionNew
                            context:(__bridge void * _Nullable)(dockTile)];
            self.defaultsObserver = hostDefaults;

            [self logMessage:[NSString stringWithFormat:@"Host defaults: %@", hostDefaults]];

            [self updateAppIcon:dockTile];
        } else {
            [self logMessage:@"No docktile, clearing icon observer"];
            [[NSDistributedNotificationCenter defaultCenter] removeObserver:self.iconChangedObserver];
            self.iconChangedObserver = nil;
            if (self.defaultsObserver) {
                [(NSUserDefaults *)self.defaultsObserver removeObserver:self forKeyPath:@"AppIcon"];
                self.defaultsObserver = nil;
            }
        }
    } @catch (NSException *exception) {
        [self logMessage:[NSString stringWithFormat:@"Exception in setDockTile: %@\nStack trace: %@\nDockTile: %@",
              exception.reason,
              exception.callStackSymbols,
              dockTile ? @"valid" : @"nil"]];
    }
}

- (void)dealloc {
    @try {
        [self logMessage:@"GalaxyDockTilePlugin deallocating"];
        if (self.iconChangedObserver) {
            [[NSDistributedNotificationCenter defaultCenter] removeObserver:self.iconChangedObserver];
            self.iconChangedObserver = nil;
        }
        if (self.defaultsObserver) {
            [(NSUserDefaults *)self.defaultsObserver removeObserver:self forKeyPath:@"AppIcon"];
            self.defaultsObserver = nil;
        }

        if (_logFileHandle) {
            [self logMessage:@"Closing log file"];
            [_logFileHandle closeFile];
            _logFileHandle = nil;
        }
    } @catch (NSException *exception) {
        NSLog(@"Exception during deallocation: %@\nStack trace: %@",
              exception.reason,
              exception.callStackSymbols);
    }
}

- (void)observeValueForKeyPath:(NSString *)keyPath
                    ofObject:(id)object
                        change:(NSDictionary<NSKeyValueChangeKey,id> *)change
                    context:(void *)context {
    @try {
        if ([keyPath isEqualToString:@"AppIcon"]) {
            [self logMessage:@"AppIcon value changed in user defaults"];
            NSDockTile *dockTile = (__bridge NSDockTile *)context;
            [self updateAppIcon:dockTile];
        }
    } @catch (NSException *exception) {
        [self logMessage:[NSString stringWithFormat:@"Exception in KVO handler: %@\nStack trace: %@\nKeyPath: %@\nObject: %@\nChange: %@",
              exception.reason,
              exception.callStackSymbols,
              keyPath,
              object,
              change]];
    }
}

@end
