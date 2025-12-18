#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const https = require('https');
const { createWriteStream } = require('fs');
const { pipeline } = require('stream/promises');

const BINARY_NAME = 'hanzo';
const REPO = 'hanzoai/cli';
const VERSION = require('./package.json').version;

function getPlatform() {
  const platform = process.platform;
  const arch = process.arch;

  const mapping = {
    'darwin-x64': 'darwin-x64',
    'darwin-arm64': 'darwin-arm64',
    'linux-x64': 'linux-x64',
    'linux-arm64': 'linux-arm64',
    'win32-x64': 'win32-x64',
  };

  const key = `${platform}-${arch}`;
  if (!mapping[key]) {
    throw new Error(`Unsupported platform: ${key}`);
  }

  return mapping[key];
}

async function downloadBinary(url, destination) {
  return new Promise((resolve, reject) => {
    https.get(url, (response) => {
      if (response.statusCode === 302 || response.statusCode === 301) {
        // Follow redirect
        downloadBinary(response.headers.location, destination)
          .then(resolve)
          .catch(reject);
        return;
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Failed to download: ${response.statusCode}`));
        return;
      }

      const file = createWriteStream(destination);
      response.pipe(file);
      file.on('finish', () => {
        file.close(resolve);
      });
    }).on('error', reject);
  });
}

async function install() {
  try {
    const platform = getPlatform();
    const binDir = path.join(__dirname, 'bin');
    const binaryPath = path.join(binDir, BINARY_NAME);

    // Create bin directory
    if (!fs.existsSync(binDir)) {
      fs.mkdirSync(binDir, { recursive: true });
    }

    // Check if we can build locally (development)
    if (fs.existsSync(path.join(__dirname, 'Cargo.toml'))) {
      console.log('Building from source...');
      try {
        execSync('cargo --version', { stdio: 'ignore' });
        console.log('Cargo found, building Hanzo CLI...');
        execSync('cargo build --release', {
          stdio: 'inherit',
          cwd: __dirname
        });

        // Copy built binary to bin directory
        const sourcePath = path.join(__dirname, 'target', 'release', BINARY_NAME);
        if (fs.existsSync(sourcePath)) {
          fs.copyFileSync(sourcePath, binaryPath);
          fs.chmodSync(binaryPath, 0o755);
          console.log('✅ Hanzo CLI built and installed successfully!');
          return;
        }
      } catch (e) {
        console.log('Cargo not found, downloading pre-built binary...');
      }
    }

    // Download pre-built binary from GitHub releases
    const downloadUrl = `https://github.com/${REPO}/releases/download/v${VERSION}/${BINARY_NAME}-${platform}`;
    console.log(`Downloading Hanzo CLI for ${platform}...`);
    console.log(`URL: ${downloadUrl}`);

    await downloadBinary(downloadUrl, binaryPath);

    // Make binary executable
    fs.chmodSync(binaryPath, 0o755);

    console.log('✅ Hanzo CLI installed successfully!');
    console.log('Run "hanzo --help" to get started');
  } catch (error) {
    console.error('Failed to install Hanzo CLI:', error.message);

    // Fallback: try to use system cargo
    console.log('\nTrying to build from source as fallback...');
    try {
      execSync('cd ' + __dirname + ' && cargo build --release', { stdio: 'inherit' });
      const sourcePath = path.join(__dirname, 'target', 'release', BINARY_NAME);
      const binDir = path.join(__dirname, 'bin');
      const binaryPath = path.join(binDir, BINARY_NAME);

      if (!fs.existsSync(binDir)) {
        fs.mkdirSync(binDir, { recursive: true });
      }

      fs.copyFileSync(sourcePath, binaryPath);
      fs.chmodSync(binaryPath, 0o755);
      console.log('✅ Successfully built from source!');
    } catch (buildError) {
      console.error('Build from source also failed:', buildError.message);
      process.exit(1);
    }
  }
}

// Run installation
install();