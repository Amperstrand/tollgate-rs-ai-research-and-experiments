include $(TOPDIR)/rules.mk

PKG_NAME:=tollgate
PKG_VERSION:=0.1.0
PKG_RELEASE:=1

# Pin to a specific commit for reproducible builds.
# Update PKG_SOURCE_VERSION and PKG_MIRROR_HASH when upgrading.
PKG_SOURCE_PROTO:=git
PKG_SOURCE_URL:=https://github.com/OpenTollGate/tollgate-rs.git
PKG_SOURCE_VERSION:=main
PKG_MIRROR_HASH:=skip

PKG_MAINTAINER:=TollGate contributors
PKG_LICENSE:=MIT
PKG_LICENSE_FILES:=LICENSE

# Rust host toolchain must be present in the build system.
# In the OpenWrt build system, enable via: make menuconfig -> Advanced -> Rust
PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

# Cargo feature flags to enable. v1-compat adds Go v1 router interoperability
# (Cashu token payments, nostr relay); spilman adds streaming payment channels.
# Override from the SDK environment with: TOLLGATE_FEATURES="v1-compat"
TOLLGATE_FEATURES?=v1-compat,spilman

include $(INCLUDE_DIR)/package.mk

# ---------------------------------------------------------------------------
# Map OpenWrt ARCH to Rust target triple.
# All OpenWrt targets use musl libc.
# ---------------------------------------------------------------------------
ifeq ($(ARCH),aarch64)
  RUST_TARGET:=aarch64-unknown-linux-musl
else ifeq ($(ARCH),x86_64)
  RUST_TARGET:=x86_64-unknown-linux-musl
else ifeq ($(ARCH),mipsel)
  RUST_TARGET:=mipsel-unknown-linux-musl
else ifeq ($(ARCH),mips)
  RUST_TARGET:=mips-unknown-linux-musl
else ifeq ($(ARCH),arm)
  # OpenWrt ARM targets predominantly use hardfloat ABI.
  # Override RUST_TARGET in your build if your target uses softfloat.
  RUST_TARGET:=arm-unknown-linux-musleabihf
else
  $(error Unsupported architecture: $(ARCH). Add a RUST_TARGET mapping in packaging/openwrt-ipk/Makefile.)
endif

RUST_RELEASE_DIR:=$(PKG_BUILD_DIR)/target/$(RUST_TARGET)/release

define Package/tollgate
  SECTION:=net
  CATEGORY:=Network
  TITLE:=TollGate — metered network access with Cashu micropayments
  URL:=https://github.com/OpenTollGate/tollgate-rs
  DEPENDS:=+nftables +ip-full +kmod-nft-tproxy
endef

define Package/tollgate/description
  TollGate is the Rust implementation of the TollGate protocol: autonomous,
  device-to-device payment for metered resource delivery, built on Cashu ecash
  and Spilman payment channels. This package installs the TollGate node daemon
  that sells metered network access over IP (and, later, over FIPS mesh).

  Two binaries are installed:
    tollgate — the TollGate node daemon (HTTP/WS transport on port 4747)
    tolltop  — live TUI dashboard for inspecting peers and channels
endef

# ---------------------------------------------------------------------------
# Build
#
# We write a temporary .cargo/config.toml to set the cross-linker without
# polluting the source tree. CARGO_HOME is scoped to the staging directory
# to avoid touching the developer's ~/.cargo during SDK builds.
# ---------------------------------------------------------------------------
define Build/Compile
	mkdir -p $(PKG_BUILD_DIR)/.cargo
	printf '[target.$(RUST_TARGET)]\nlinker = "$(TARGET_CC)"\n' \
		> $(PKG_BUILD_DIR)/.cargo/config.toml
	cd $(PKG_BUILD_DIR) && \
	CARGO_HOME=$(STAGING_DIR_HOST)/share/cargo \
	cargo build \
		--release \
		--target $(RUST_TARGET) \
		--features $(TOLLGATE_FEATURES) \
		--bin tollgate \
		--bin tolltop
endef

define Package/tollgate/install
	# Binaries
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(RUST_RELEASE_DIR)/tollgate $(1)/usr/bin/tollgate
	$(INSTALL_BIN) $(RUST_RELEASE_DIR)/tolltop  $(1)/usr/bin/tolltop

	# procd init script
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) $(CURDIR)/files/etc/init.d/tollgate $(1)/etc/init.d/tollgate

	# Default config — installed as CONF so opkg will not overwrite it on upgrade
	$(INSTALL_DIR) $(1)/etc/tollgate
	$(INSTALL_CONF) $(CURDIR)/files/etc/tollgate/tollgate.yaml.example $(1)/etc/tollgate/tollgate.yaml

	# UCI config defaults (optional runtime knobs)
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_DATA) $(CURDIR)/files/etc/config/tollgate $(1)/etc/config/tollgate

	# Firewall include config (lan-zone accept rules for the TollGate port)
	$(INSTALL_DATA) $(CURDIR)/files/etc/config/firewall-tollgate $(1)/etc/config/firewall-tollgate

	# Hotplug: restart TollGate when WAN comes up with connectivity
	$(INSTALL_DIR) $(1)/etc/hotplug.d/iface
	$(INSTALL_BIN) $(CURDIR)/files/etc/hotplug.d/iface/95-tollgate-restart $(1)/etc/hotplug.d/iface/95-tollgate-restart

	# UCI defaults: one-time first-boot firewall/module/config setup
	$(INSTALL_DIR) $(1)/etc/uci-defaults
	$(INSTALL_BIN) $(CURDIR)/files/etc/uci-defaults/90-tollgate-firewall $(1)/etc/uci-defaults/90-tollgate-firewall
	$(INSTALL_BIN) $(CURDIR)/files/etc/uci-defaults/99-tollgate-setup     $(1)/etc/uci-defaults/99-tollgate-setup

	# sysupgrade: preserve /etc/tollgate/ (config + identity key) across firmware upgrades
	$(INSTALL_DIR) $(1)/lib/upgrade/keep.d
	$(INSTALL_DATA) $(CURDIR)/files/lib/upgrade/keep.d/tollgate $(1)/lib/upgrade/keep.d/tollgate
endef

$(eval $(call BuildPackage,tollgate))
