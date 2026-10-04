"""Offline upgrade-helper tests. Never execute APT, services or a release upgrade."""
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "src-tauri/scripts/upgrade_noland_vm.sh"


def bash(body):
    return subprocess.run(["bash", "-c", f'source "{SCRIPT}"\n{body}'], text=True, capture_output=True)


class UpgradeToolTests(unittest.TestCase):
    def test_syntax_and_help(self):
        for name in ("upgrade_noland_vm.sh", "install_vm_upgrade_tool.sh"):
            subprocess.run(["bash", "-n", str(SCRIPT.with_name(name))], check=True)
        result = subprocess.run(["bash", str(SCRIPT), "--help"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn("22.04 -> 24.04", result.stdout)

    def test_selects_official_noble_version_over_newer_jammy_or_ppa(self):
        result = bash('''
apt-cache() { cat <<'EOF'
libspa | 1.0.7-3~ubuntu22.04 | http://archive.ubuntu.com/ubuntu jammy/main amd64 Packages
libspa | 9.0.0 | https://ppa.launchpadcontent.net/vendor/repo/ubuntu noble/main amd64 Packages
libspa | 1.0.5-1ubuntu3 | http://archive.ubuntu.com/ubuntu noble-updates/main amd64 Packages
libspa | 1.0.5-1ubuntu3.3 | http://security.ubuntu.com/ubuntu noble-security/main amd64 Packages
EOF
}
noble_version libspa
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "1.0.5-1ubuntu3.3")

    def test_missing_noble_version_stops_before_install(self):
        result = bash('''
apt-cache() { echo 'package | 1.0.7 | http://archive.ubuntu.com/ubuntu jammy/main amd64 Packages'; }
apt_run() { echo 'UNEXPECTED_INSTALL'; }
repair_packages
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("UNEXPECTED_INSTALL", result.stdout)

    def test_repair_simulates_before_install_and_aligns_installed_plugins(self):
        result = bash('''
noble_version() { echo 1.0.5; }
dpkg-query() { echo 'install ok installed'; }
apt_run() { printf 'APT %s\\n' "$*"; }
dpkg() { :; }
test() { :; }
apt-mark() { printf 'MARK %s\\n' "$*"; }
repair_packages
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertIn('-s --fix-broken --allow-downgrades --no-remove install', lines[0])
        self.assertIn('-y --fix-broken --allow-downgrades --no-remove install', lines[1])
        for package in ("libspa-0.2-bluetooth", "libspa-0.2-jack", "pipewire-alsa", "pipewire-pulse"):
            self.assertIn(f"{package}=1.0.5", lines[1])
        desktop_simulation = next(line for line in lines if "-s " in line and "plasma-workspace=" in line)
        desktop_install = next(line for line in lines if "-y " in line and "plasma-workspace=" in line)
        self.assertGreater(lines.index(desktop_simulation), lines.index(lines[1]))
        for package in ("qml-module-org-kde-pipewire", "libkpipewire5", "libkpipewiredmabuf5", "libkpipewirerecord5"):
            self.assertIn(f"{package}=1.0.5", desktop_install)
        self.assertNotIn("plasma-workspace=", lines[1])
        self.assertIn('MARK manual sunshine plasma-workspace plasma-desktop kwin-x11 pipewire pipewire-pulse wireplumber', lines)

    def test_desktop_solver_failure_does_not_block_library_repair_or_install_kde(self):
        result = bash("""
noble_version() { echo 1.0.5; }
dpkg-query() { echo 'install ok installed'; }
dpkg() { :; }
apt_run() {
  printf 'APT %s\\n' "$*"
  if [[ "$*" == *plasma-workspace* && "$*" == -s* ]]; then return 100; fi
}
repair_packages
""")
        self.assertEqual(result.returncode, 100)
        installs = [line for line in result.stdout.splitlines() if line.startswith('APT -y')]
        self.assertEqual(len(installs), 1)
        self.assertIn('libpipewire-0.3-0t64=', installs[0])
        self.assertNotIn('plasma-workspace=', installs[0])

    def test_failed_simulation_prevents_real_install(self):
        result = bash('''
noble_version() { echo 1.0.5; }
dpkg-query() { [[ "$*" == *sunshine* ]] && echo 'install ok installed'; }
apt_run() { printf 'APT %s\\n' "$*"; return 1; }
repair_packages
''')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('APT -y', result.stdout)

    def test_stale_wine_sources_are_disabled_without_changing_noble_entries(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            sources = root / "sources.list.d"
            sources.mkdir()
            old = sources / "wine.sources"
            old.write_text("Types: deb\nURIs: https://dl.winehq.org/wine-builds/ubuntu\nSuites: jammy\nComponents: main\n")
            current = sources / "current.list"
            current.write_text("deb https://dl.winehq.org/wine-builds/ubuntu noble main\n")
            copy = root / "helper.sh"
            copy.write_text(SCRIPT.read_text().replace('/etc/apt', str(root)))
            command = f'source "{copy}"; disable_stale_wine_source; disable_stale_wine_source'
            result = subprocess.run(["bash", "-c", command], text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(old.read_text().count("Enabled: no"), 1)
            self.assertEqual(current.read_text(), "deb https://dl.winehq.org/wine-builds/ubuntu noble main\n")

    def test_installer_creates_desktop_tools_and_restricted_sudo_rule(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            home = root / "home"
            (root / "etc/sudoers.d").mkdir(parents=True)
            installer = SCRIPT.with_name("install_vm_upgrade_tool.sh").read_text()
            installer = installer.replace('[[ $EUID == 0 ]]', 'true')
            installer = installer.replace('/usr/local/lib/noland', f'{root}/lib/noland')
            installer = installer.replace('/etc/sudoers.d', f'{root}/etc/sudoers.d')
            mocks = f"""
id() {{ echo testuser; }}
chown() {{ :; }}
visudo() {{ :; }}
install() {{
  local args=()
  while [[ $# -gt 0 ]]; do
    case "$1" in -o|-g) shift 2 ;; *) args+=("$1"); shift ;; esac
  done
  /usr/bin/install "${{args[@]}}"
}}
TEST_HOME='{home}'
getent() {{ printf 'testuser:x:1000:1000::%s:/bin/bash\\n' "$TEST_HOME"; }}
set -- testuser '{SCRIPT}' '{SCRIPT.with_name('change_display_resolution.py')}'
"""
            result = subprocess.run(["bash", "-c", mocks + installer], text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            tools = home / "Desktop/tools"
            self.assertTrue((tools / "Upgrade Ubuntu.desktop").exists())
            self.assertTrue((tools / "README.txt").exists())
            self.assertTrue((tools / "Change Display Resolution.desktop").exists())
            self.assertNotIn("sudo", (tools / "change-display-resolution.sh").read_text())
            self.assertIn('exec sudo', (tools / "upgrade-noland-vm.sh").read_text())
            self.assertEqual((root / "lib/noland/upgrade-vm.sh").read_text(), SCRIPT.read_text())
            rule = (root / "etc/sudoers.d/noland-vm-upgrade-testuser").read_text()
            self.assertIn('upgrade-vm.sh "",', rule)
            self.assertIn('upgrade-vm.sh --repair', rule)
            self.assertNotIn('ALL\n', rule)

    def test_repair_reboots_then_verifies_without_repeating_release_upgrade(self):
        with tempfile.TemporaryDirectory() as folder:
            state = pathlib.Path(folder)
            (state / 'phase').write_text('repair')
            (state / 'user').write_text('root')
            result = bash(f'''
STATE='{state}'
os_codename() {{ echo noble; }}
apt_run() {{ :; }}
disable_stale_wine_source() {{ :; }}
repair_packages() {{ echo REPAIR_PACKAGES; }}
repair_audio() {{ echo REPAIR_AUDIO; }}
systemctl() {{ echo "SERVICE $*"; }}
worker
''')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((state / 'phase').read_text(), 'verify')
            self.assertIn('SERVICE reboot', result.stdout)
            (state / 'reboot-from').write_text('previous-boot')
            result = bash(f'''
STATE='{state}'
os_codename() {{ echo noble; }}
disable_stale_wine_source() {{ :; }}
apt_run() {{ :; }}
repair_packages() {{ echo REPAIR_PACKAGES_AFTER_BOOT; }}
verify() {{ echo VERIFIED; }}
systemctl() {{ echo "SERVICE $*"; }}
worker
''')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((state / 'phase').read_text(), 'complete')
            self.assertIn('VERIFIED', result.stdout)
            self.assertLess(result.stdout.index('REPAIR_PACKAGES_AFTER_BOOT'), result.stdout.index('VERIFIED'))
            self.assertIn('SERVICE disable noland-distro-upgrade.service', result.stdout)

    def test_post_reboot_package_failure_never_marks_upgrade_complete(self):
        with tempfile.TemporaryDirectory() as folder:
            state = pathlib.Path(folder)
            (state / 'phase').write_text('verify')
            (state / 'user').write_text('root')
            (state / 'reboot-from').write_text('previous-boot')
            result = bash(f'''
STATE='{state}'
os_codename() {{ echo noble; }}
disable_stale_wine_source() {{ :; }}
apt_run() {{ :; }}
repair_packages() {{ return 100; }}
verify() {{ echo UNEXPECTED_VERIFY; }}
systemctl() {{ echo "UNEXPECTED_SERVICE $*"; }}
worker
''')
            self.assertEqual(result.returncode, 100)
            self.assertEqual((state / 'phase').read_text(), 'failed')
            self.assertNotIn('UNEXPECTED_', result.stdout)


if __name__ == "__main__":
    unittest.main()
