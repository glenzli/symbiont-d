APP_NAME := SymbiontMenu.app
BUILD_DIR := build
BUNDLE_DIR := $(BUILD_DIR)/$(APP_NAME)
BINARY := $(BUNDLE_DIR)/Contents/MacOS/SymbiontMenu
INFO_PLIST := $(BUNDLE_DIR)/Contents/Info.plist
TEST_BINARY := $(BUILD_DIR)/SMMenuStateTests
BUNDLE_RESOURCES := \
	$(BUNDLE_DIR)/Contents/Resources/AppIcon.icns \
	$(BUNDLE_DIR)/Contents/Resources/MenuBarIcon.png \
	$(BUNDLE_DIR)/Contents/Resources/MenuBarIcon@2x.png
SOURCES := $(wildcard Sources/*.m)
HEADERS := $(wildcard Sources/*.h)

.PHONY: all app clean test verify

all: app

app: $(BINARY) $(INFO_PLIST) $(BUNDLE_RESOURCES)
	/usr/bin/codesign --force --sign - "$(BUNDLE_DIR)"

$(BINARY): $(SOURCES) $(HEADERS)
	mkdir -p "$(dir $@)"
	xcrun clang -fobjc-arc -fmodules -Wall -Wextra -Werror \
		-fmodules-cache-path="$(BUILD_DIR)/ModuleCache" \
		-mmacosx-version-min=13.0 \
		-framework AppKit -framework WebKit \
		$(SOURCES) -o "$@"

$(INFO_PLIST): Resources/Info.plist
	mkdir -p "$(dir $@)"
	cp "$<" "$@"
	plutil -lint "$@" >/dev/null

$(BUNDLE_DIR)/Contents/Resources/%: Resources/%
	mkdir -p "$(dir $@)"
	cp "$<" "$@"

$(TEST_BINARY): Tests/SMMenuStateTests.m Sources/SMMenuState.m Sources/SMMenuState.h
	mkdir -p "$(dir $@)"
	xcrun clang -fobjc-arc -fmodules -Wall -Wextra -Werror \
		-fmodules-cache-path="$(BUILD_DIR)/ModuleCache" \
		-mmacosx-version-min=13.0 -framework Foundation -I Sources \
		Tests/SMMenuStateTests.m Sources/SMMenuState.m -o "$@"

test: $(TEST_BINARY)
	"$(TEST_BINARY)"

verify: app test
	/usr/bin/codesign --verify --strict "$(BUNDLE_DIR)"
	plutil -lint "$(INFO_PLIST)"

clean:
	rm -rf "$(BUILD_DIR)"
