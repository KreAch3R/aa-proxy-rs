use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kobject_uevent::UEvent;
use netlink_sys::protocols::NETLINK_KOBJECT_UEVENT;
use simplelog::*;
use std::process;
use tokio::time::timeout;

// module name for logging engine
const NAME: &str = "<i><bright-black> usb: </>";

pub const DEFAULT_GADGET_NAME: &str = "default";
pub const ACCESSORY_GADGET_NAME: &str = "accessory";

pub fn uevent_listener(accessory_started: Arc<tokio::sync::Notify>) {
    info!("{} 📬 Starting UEvent listener thread...", NAME);
    let mut socket = netlink_sys::Socket::new(NETLINK_KOBJECT_UEVENT).unwrap();
    let sa = netlink_sys::SocketAddr::new(process::id(), 1);
    let mut buf = vec![0u8; 1024 * 8];

    socket.bind(&sa).unwrap();

    loop {
        let _ = socket.recv(&mut buf, 0).unwrap();
        let u = UEvent::from_netlink_packet(&buf).unwrap();
        if u.env.get("DEVNAME").is_some_and(|x| x == "usb_accessory")
            && u.env.get("ACCESSORY").is_some_and(|x| x == "START")
        {
            debug!("got uevent: {:#?}", u);
            accessory_started.notify_one();
        }
    }
}

pub fn write_data(output_path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = fs::File::create(output_path)?;
    f.write_all(data)?;

    Ok(())
}

pub struct UsbGadgetState {
    configfs_path: PathBuf,
    udc_name: String,
}

impl UsbGadgetState {
    pub fn new() -> UsbGadgetState {
        let mut state = UsbGadgetState {
            configfs_path: PathBuf::from("/sys/kernel/config/usb_gadget"),
            udc_name: String::new(),
        };

        let udc_dir = PathBuf::from("/sys/class/udc");
        if let Ok(entries) = fs::read_dir(&udc_dir) {
            for entry in entries {
                if let Ok(entry) = entry {
                    debug!("Using UDC: {:?}", entry.file_name());
                    if let Ok(fname) = entry.file_name().into_string() {
                        state.udc_name.push_str(fname.as_str());
                        break;
                    }
                }
            }
        }

        return state;
    }

    pub fn init(&mut self) {
        info!("{} 🔌 Initializing USB Manager", NAME);
        let _ = self.disable(DEFAULT_GADGET_NAME);
        let _ = self.disable(ACCESSORY_GADGET_NAME);
        info!("{} 🔌 USB Manager: Disabled all USB gadgets", NAME);
    }

    pub async fn enable_default_and_wait_for_accessory(
        &mut self,
        accessory_started: Arc<tokio::sync::Notify>,
    ) {
        for _try in 1..=2 {
            let _ = self.enable(DEFAULT_GADGET_NAME);
            info!("{} 🔌 USB Manager: Enabled default gadget", NAME);

            // now waiting for accesory start from uevent thread loop
            let retval = accessory_started.notified();
            if let Err(_) = timeout(Duration::from_secs_f32(3.0), retval).await {
                error!(
                    "{} 🔌 USB Manager: Timeout waiting for accessory start, trying to recover...",
                    NAME
                );
            } else {
                break;
            };
        }

        info!("{} 🔌 USB Manager: Received accessory start request", NAME);
        let _ = self.disable(DEFAULT_GADGET_NAME);
        // 0.1 second, keep the gadget disabled for a short time to let the host recognize the change
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = self.enable(ACCESSORY_GADGET_NAME);
        info!(
            "{} 🔌 USB Manager: Switched from default to accessory gadget",
            NAME
        );
    }

    fn enable(&mut self, gadget_name: &str) -> io::Result<()> {
        if !self.configfs_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "ConfigFs path does not exist",
            ));
        }

        let gadget_path = self.configfs_path.join(gadget_name);
        write_data(gadget_path.join("UDC").as_path(), self.udc_name.as_bytes())?;

        Ok(())
    }

    fn disable(&mut self, gadget_name: &str) -> io::Result<()> {
        if !self.configfs_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "ConfigFs path does not exist",
            ));
        }

        let gadget_path = self.configfs_path.join(gadget_name);
        write_data(gadget_path.join("UDC").as_path(), "\n".as_bytes())?;

        Ok(())
    }
}
