use std::{thread, time::Duration};

use hidapi::{HidApi, HidDevice, HidResult};
use vigem_client::XButtons;

const REPORT_LEN: usize = 48;
pub struct ProController {
    pub hid: HidDevice,
    global_count: u8,
    lstick_cal: [u16; 6],
    rstick_cal: [u16; 6],
    ldeadzone: u16,
    rdeadzone: u16
} 

trait Controller {
    fn find_device() -> Result<HidDevice, String>;
    fn center_sticks(vals: [u16; 2], cal: [u16; 6], dz: u16) -> [f32; 2];
    fn new(hid: HidDevice) -> Self;

    fn subcommand(&mut self, sc: u8, send: &[u8], recv: &mut [u8]) -> Result<(), String>;
    fn void_subcommand(&mut self, sc: u8, send: &[u8]) -> Result<(), String>;
    fn attach(&mut self) -> Result<(), String>;
    fn reset(&mut self) -> Result<(), String>;
    fn x80_write(&self, buf: &mut [u8; 64], code: u8) -> Result<(), String>;
    fn handshake(&self) -> Result<(), String>;
    fn read_spi(&mut self, from: i32, size: u8) -> Result<Vec<u8>, String>;
    fn read_stick_calibration(&mut self, user_address: i32, factory_address: i32, deadzone_address: i32, side: bool) -> Result<u16, String>;
    fn calibrate(&mut self) -> Result<(), String>;
    fn set_imu(&mut self, state: bool) -> Result<(), String>;
    fn set_vibration(&mut self, state: bool) -> Result<(), String>;
    fn set_report_mode(&mut self, mode: u8) -> Result<(), String>;
    fn set_player_lights(&mut self, bitfield: u8) -> Result<(), String>;
    fn read_hid(&self, buf: &mut [u8]) -> HidResult<usize>;
}

// From https://github.com/Davidobot/BetterJoy/blob/461f5f8f5c0368eeae8dfdf27536bc8cb906ac19/BetterJoyForCemu/Joycon.cs
// Lots of help from https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering/
impl Controller for ProController {
    fn find_device() -> Result<HidDevice, String> {
        let api = HidApi::new().expect("Couldn't open HidApi");
        let mut device_result = None;
        for device in api.device_list() {
            let name = device.product_string().unwrap_or("");
            if name.to_lowercase().starts_with("pro controller") {
                device_result = Some(device.open_device(&api));
                break;
            }
        }
        let device = device_result.expect("Couldn't find Pro Controller").expect("Couldn't open Pro Controller device");
        device.set_blocking_mode(true).expect("Couldn't set device to blocking mode");
        return Ok(device)
    }

    fn center_sticks(vals: [u16; 2], cal: [u16; 6], dz: u16) -> [f32; 2] {
        let t = cal;
        let mut s = [0f32,0.0];
        let dx: f32 = f32::from(vals[0]) - f32::from(t[2]);
        let dy: f32 = f32::from(vals[1]) - f32::from(t[3]);
        
        if (dx * dx + dy * dy).abs() < f32::from(dz * dz) {
            return s
        }
        s[0] = dx / if dx > 0.0 { f32::from(t[0]) } else { f32::from(t[4]) };
        s[1] = dy / if dy > 0.0 { f32::from(t[1]) } else { f32::from(t[5]) };
        return s
    }

    fn new(hid: HidDevice) -> Self {
        let lstick_cal = [0u16; 6];
        let rstick_cal = [0u16; 6];
        
        return ProController {
            hid: hid,
            global_count: 0,
            lstick_cal: lstick_cal,
            rstick_cal: rstick_cal,
            ldeadzone: 0,
            rdeadzone: 0
        }
    }

    fn subcommand(&mut self, sc: u8, send: &[u8], recv: &mut [u8]) -> Result<(), String> {
        let default_buf: [u8; 8] = [0x0, 0x1, 0x40, 0x40, 0x0, 0x1, 0x40, 0x40];
        let mut buf_ = [0u8; REPORT_LEN];
        buf_[2..10].copy_from_slice(&default_buf);
        buf_[11..(11+send.len())].copy_from_slice(send);
        buf_[10] = sc;
        buf_[1] = self.global_count;
        buf_[0] = 0x1;
        if self.global_count == 0xf {
            self.global_count = 0;
        } else {
            self.global_count += 1;
        }
        if let Err(e) = self.hid.write(&buf_) {
            return Err(e.to_string());
        }
        let mut tries = 0;
        let mut result;
        loop {
            result = self.hid.read_timeout(recv, 100);
            tries += 1;
            if !(tries < 10 && recv[0] != 0x21 && recv[14] != sc) {
                break;
            }
        }
        if result.is_err() {
            return Err(result.err().unwrap().to_string())
        }
        return Ok(())
    }

    fn void_subcommand(&mut self, sc: u8, send: &[u8]) -> Result<(), String> {
        return self.subcommand(sc, send, &mut [0u8; 16]);
    }

    fn reset(&mut self) -> Result<(), String> {        
        return self.void_subcommand(0x06, &[0x04])
    }

    fn x80_write(&self, buf: &mut [u8; 64], code: u8) -> Result<(), String> {
        buf[0] = 0x80;
        buf[1] = code;
        if let Err(e) = self.hid.write(buf) {
            return Err(e.to_string())
        }
        let _ = self.hid.read_timeout(&mut [], 100);
        return Ok(());
    }

    fn handshake(&self) -> Result<(), String> {
        let mut buf = [0u8; 64];
        // Handshake
        self.x80_write(&mut buf, 0x2)?;
        // 3Mbit Baudrate
        self.x80_write(&mut buf, 0x3)?;
        // Handshake again
        self.x80_write(&mut buf, 0x2)?;
        // Force USB HID only
        self.x80_write(&mut buf, 0x4)?;
        return Ok(())
    }

    fn read_spi(&mut self, from: i32, size: u8) -> Result<Vec<u8>, String> {
        if size > 0x1d {
            return Err(format!("Reading size {size} > 0x1d"));
        }
        let mut cmd = [0xff, 0xff, 0x00, 0x00, size];
        cmd[0..4].copy_from_slice(&from.to_le_bytes());
        let mut buf_ = [0u8; REPORT_LEN];
        self.subcommand(0x10, &cmd, &mut buf_)?;
        let res = buf_[20..(20+usize::from(size))].to_owned();
        // let mut addr = [0u8; 4];
        // addr[0] = buf_[0];
        // addr[1] = buf_[1];
        // addr[2] = buf_[2];
        // addr[3] = buf_[3];
        // println!("read_spi from {:#06x} got {:#06x} size {size}", from, i32::from_le_bytes(addr));
        return Ok(res)
    }

    fn read_stick_calibration(&mut self, user_address: i32, factory_address: i32, deadzone_address: i32, side: bool) -> Result<u16, String> {
        let mut buf_ = self.read_spi(user_address, 9)?;
        let mut found = false;
        let side_name = if side { "Left" } else { "Right" };
        for i in buf_.iter() {
            if *i == 0xff || *i == 0x00 {
                continue;
            }
            println!("Using user calibration data for {side_name}");
            found = true;
        }
        if !found {
            println!("Using factory calibration data for {side_name}");
            buf_ = self.read_spi(factory_address, 9)?;
        }
        let stick_cal = if side { &mut self.lstick_cal } else { &mut self.rstick_cal };
        let stick_indexes = if side { [0usize, 1, 2, 3, 4, 5] } else { [2, 3, 4, 5, 0, 1] };
        stick_cal[stick_indexes[0]] = (u16::from(buf_[1]) << 8) & 0xF00 | u16::from(buf_[0]);
        stick_cal[stick_indexes[1]] = (u16::from(buf_[2]) << 4) | (u16::from(buf_[1]) >> 4);
        stick_cal[stick_indexes[2]] = (u16::from(buf_[4]) << 8) & 0xF00 | u16::from(buf_[3]);
        stick_cal[stick_indexes[3]] = (u16::from(buf_[5]) << 4) | (u16::from(buf_[4]) >> 4);
        stick_cal[stick_indexes[4]] = (u16::from(buf_[7]) << 8) & 0xF00 | u16::from(buf_[6]);
        stick_cal[stick_indexes[5]] = (u16::from(buf_[8]) << 4) | (u16::from(buf_[7]) >> 4);
        buf_ = self.read_spi(deadzone_address, 16)?;
        let deadzone = (u16::from(buf_[4]) << 8) & 0xF00 | u16::from(buf_[3]);
        return Ok(deadzone);
    }

    fn calibrate(&mut self) -> Result<(), String> {
        self.ldeadzone = self.read_stick_calibration(0x8012, 0x603d, 0x6086, true)?;
        self.rdeadzone = self.read_stick_calibration(0x801d, 0x6046, 0x6098, false)?;
        return Ok(());
    }

    fn set_imu(&mut self, state: bool) -> Result<(), String> {
        return self.void_subcommand(0x40, &[if state { 0x01 } else { 0x00 }]);
    }

    fn set_vibration(&mut self, state: bool) -> Result<(), String> {
        return self.void_subcommand(0x48, &[if state { 0x01 } else { 0x00 }]);
    }

    fn set_report_mode(&mut self, mode: u8) -> Result<(), String> {
        return self.void_subcommand(0x03, &[mode]);
    }

    fn set_player_lights(&mut self, bitfield: u8) -> Result<(), String> {
        return self.void_subcommand(0x30, &[bitfield]);
    }

    fn read_hid(&self, buf: &mut [u8]) -> HidResult<usize> {
        return self.hid.read(buf);
    }

    fn attach(&mut self) -> Result<(), String> {
        self.global_count = 0;
        if let Err(e) = self.hid.write(&[0x80, 0x1]) {
            return Err(e.to_string());
        }
        let mut buf = [0u8; 256];
        let read = self.hid.read_timeout(&mut buf[..], 100);
        if read.is_err() {
            return Err(read.err().unwrap().to_string())
        }
        if buf[0] != 0x81 {
            self.reset()?;
            thread::sleep(Duration::from_millis(6000));
            self.hid = ProController::find_device()?;
            // !! Simple hid to make sure we catch 0x81 next time !!
            self.set_report_mode(0x3f)?;
            self.attach()?;
            return Ok(());            
        }
        self.handshake()?;
        self.calibrate()?;
        self.set_imu(false)?;
        self.set_vibration(false)?;
        self.set_player_lights(0b00001000)?;
        // 60hz
        self.set_report_mode(0x30)?;
        return Ok(())
    }
}

fn main() {
    let client = vigem_client::Client::connect().unwrap();

    let id = vigem_client::TargetId::XBOX360_WIRED;
    let mut target = vigem_client::Xbox360Wired::new(client, id);

    target.plugin().unwrap();
    target.wait_ready().unwrap();

    let mut gamepad = vigem_client::XGamepad {
        ..Default::default()
    };

    let device = ProController::find_device().expect("Couldn't find Pro Controller");
    let mut controller = ProController::new(device);
    if let Err(e) = controller.attach() {
        println!("Error while attaching: {e}");
        return;
    };
    println!("Left Deadzone: {}, Left Calibration: {:?}", controller.ldeadzone, controller.lstick_cal);
    println!("Right Deadzone: {}, Right Calibration: {:?}", controller.rdeadzone, controller.rstick_cal);
    println!("Successfully attached to Pro Controller!");
    
    loop {
        let mut data = [0u8; REPORT_LEN];
        let _res = controller.read_hid(&mut data[..]).expect("Couldn't read hid!");
        let left_side = data[3];
        let right_trigger = u16::from(left_side  & 0b10000000);
        let right_shoulder = u16::from(left_side & 0b01000000);
        let a_button = u16::from(left_side      & 0b00001000);
        let b_button = u16::from(left_side      & 0b00000100);
        let x_button = u16::from(left_side      & 0b00000010);
        let y_button = u16::from(left_side      & 0b00000001);

        let other_buttons = data[4];
        let _capture = u16::from(other_buttons       & 0b00100000);
        let _home = u16::from(other_buttons          & 0b00010000);
        let _lstick_button = u16::from(other_buttons & 0b00001000);
        let _rstick_button = u16::from(other_buttons & 0b00000100);
        let plus = u16::from(other_buttons          & 0b00000010);
        let minus = u16::from(other_buttons         & 0b00000001);

        let right_side = data[5];
        let left_trigger = u16::from(right_side  & 0b10000000);
        let left_shoulder = u16::from(right_side & 0b01000000);
        let dpad_left = u16::from(right_side      & 0b00001000);
        let dpad_right = u16::from(right_side     & 0b00000100);
        let dpad_up = u16::from(right_side        & 0b00000010);
        let dpad_down = u16::from(right_side      & 0b00000001);


        let lstick_raw = [data[6], data[7], data[8]];
        let lstick_precal = [
            (u16::from(lstick_raw[0]) | ((u16::from(lstick_raw[1]) & 0xf) << 8)), 
            ((u16::from(lstick_raw[1]) >> 4) | (u16::from(lstick_raw[2]) << 4))
        ];
        let lstick = ProController::center_sticks(lstick_precal, controller.lstick_cal, controller.ldeadzone);
        let rstick_raw = [data[6+3], data[7+3], data[8+3]];
        let rstick_precal = [
            (u16::from(rstick_raw[0]) | ((u16::from(rstick_raw[1]) & 0xf) << 8)), 
            ((u16::from(rstick_raw[1]) >> 4) | (u16::from(rstick_raw[2]) << 4))
        ];
        let rstick = ProController::center_sticks(rstick_precal, controller.rstick_cal, controller.rdeadzone);
        
        gamepad.thumb_lx = (32767f32 * lstick[0]) as i16;
        gamepad.thumb_ly = (32767f32 * lstick[1]) as i16;
        gamepad.thumb_rx = (32767f32 * rstick[0]) as i16;
        gamepad.thumb_ry = (32767f32 * rstick[1]) as i16;
        gamepad.buttons = XButtons::from(
            a_button << 9 | b_button << 11 | x_button << 13 | y_button << 15 
            | dpad_up >> 1 | dpad_down << 1 | dpad_left >> 1 | dpad_right << 1
            | plus << 3 | minus << 5 
            // | right_trigger >> 1 | left_trigger
            | left_shoulder << 2 | right_shoulder << 3
        );
        gamepad.left_trigger = if left_trigger != 0 { 255 } else { 0 };
        gamepad.right_trigger = if right_trigger != 0 { 255 } else { 0 };
        target.update(&gamepad).unwrap_or(());
    }
}
