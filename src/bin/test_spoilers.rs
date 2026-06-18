use std::ffi::{c_char, c_void};
use std::thread;
use std::time::{Duration, Instant};
use libloading::Library;

type DWord = u32;
type HRESULT = i32;
type Handle = *mut c_void;
type HWnd = *mut c_void;

#[repr(C)]
struct SimRecv {
    dw_size: DWord,
    dw_version: DWord,
    dw_id: DWord,
}

#[repr(C)]
struct SimRecvSimObjectData {
    base: SimRecv,
    dw_request_id: DWord,
    dw_object_id: DWord,
    dw_define_id: DWord,
    dw_flags: DWord,
    dw_entrynumber: DWord,
    dw_outof: DWord,
    dw_define_count: DWord,
    dw_data: DWord,
}

const SIMCONNECT_RECV_ID_QUIT: DWord = 3;
const SIMCONNECT_RECV_ID_SIMOBJECT_DATA: DWord = 8;
const SIMCONNECT_PERIOD_SIM_FRAME: DWord = 3;
const SIMCONNECT_DATATYPE_FLOAT64: DWord = 4;
const USER_OBJECT_ID: DWord = 0;

type PfnSimConnectOpen = unsafe extern "system" fn(*mut Handle, *const c_char, HWnd, DWord, Handle, DWord) -> HRESULT;
type PfnSimConnectClose = unsafe extern "system" fn(Handle) -> HRESULT;
type PfnSimConnectAddToDataDefinition = unsafe extern "system" fn(Handle, DWord, *const c_char, *const c_char, DWord, f32, DWord) -> HRESULT;
type PfnSimConnectRequestDataOnSimObject = unsafe extern "system" fn(Handle, DWord, DWord, DWord, DWord, DWord, DWord, DWord, DWord) -> HRESULT;
type PfnSimConnectGetNextDispatch = unsafe extern "system" fn(Handle, *mut *mut SimRecv, *mut DWord) -> HRESULT;

fn main() {
    println!("=== ТЕСТ СЛУШАТЕЛЯ СПОЙЛЕРОВ SimConnect ===");

    // Загружаем SimConnect.dll напрямую из папки lib проекта
    let lib = unsafe { 
        Library::new("lib/SimConnect.dll")
            .or_else(|_| Library::new("SimConnect.dll")) 
    }.expect("Не удалось найти SimConnect.dll! Проверьте, что папка lib/SimConnect.dll существует.");
    
    let open_fn: PfnSimConnectOpen = unsafe { *lib.get(b"SimConnect_Open\0").unwrap() };
    let close_fn: PfnSimConnectClose = unsafe { *lib.get(b"SimConnect_Close\0").unwrap() };
    let add_to_def_fn: PfnSimConnectAddToDataDefinition = unsafe { *lib.get(b"SimConnect_AddToDataDefinition\0").unwrap() };
    let req_data_fn: PfnSimConnectRequestDataOnSimObject = unsafe { *lib.get(b"SimConnect_RequestDataOnSimObject\0").unwrap() };
    let next_dispatch_fn: PfnSimConnectGetNextDispatch = unsafe { *lib.get(b"SimConnect_GetNextDispatch\0").unwrap() };

    let mut h_sc: Handle = std::ptr::null_mut();
    let app_name = std::ffi::CString::new("SpoilerTest").unwrap();
    
    let hr = unsafe { open_fn(&mut h_sc, app_name.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut(), 0) };
    if hr < 0 || h_sc.is_null() {
        println!("Ошибка подключения к симулятору! Код: 0x{:08X}", hr as u32);
        return;
    }
    println!("Успешно подключено к SimConnect!");

    // Регистрируем разные варианты спойлеров
    let defs = [
        ("SPOILERS HANDLE POSITION", "Position"),  // 0: формат 0.0 .. 1.0
        ("SPOILERS HANDLE POSITION", "Percent"),   // 1: формат 0 .. 100%
        ("SPOILERS LEFT POSITION", "Percent"),     // 2: положение левой плоскости
        ("SPOILERS RIGHT POSITION", "Percent"),    // 3: положение правой плоскости
        ("SPOILERS POSITION", "Position"),         // 4: общее положение плоскостей
    ];

    let def_id = 100u32;
    let req_id = 200u32;

    for (name, unit) in &defs {
        let n = std::ffi::CString::new(*name).unwrap();
        let u = std::ffi::CString::new(*unit).unwrap();
        unsafe {
            add_to_def_fn(h_sc, def_id, n.as_ptr(), u.as_ptr(), SIMCONNECT_DATATYPE_FLOAT64, 0.0, 0xFFFF_FFFF);
        }
        println!("Добавлено в подписку: {} [{}]", name, unit);
    }

    // Запрашиваем данные каждый кадр симулятора
    unsafe {
        req_data_fn(h_sc, req_id, def_id, USER_OBJECT_ID, SIMCONNECT_PERIOD_SIM_FRAME, 0, 0, 0, 0);
    }
    println!("\nНачинаем чтение данных... Пошевелите рычаг спойлеров в кабине лайнера!\n");

    let mut last_print = Instant::now();

    loop {
        let mut p_recv: *mut SimRecv = std::ptr::null_mut();
        let mut cb: DWord = 0;
        let hr = unsafe { next_dispatch_fn(h_sc, &mut p_recv, &mut cb) };

        if hr >= 0 && !p_recv.is_null() {
            unsafe {
                if (*p_recv).dw_id == SIMCONNECT_RECV_ID_QUIT {
                    println!("Симулятор закрылся.");
                    break;
                }

                if (*p_recv).dw_id == SIMCONNECT_RECV_ID_SIMOBJECT_DATA {
                    let sod = &*(p_recv as *const SimRecvSimObjectData);
                    if sod.dw_request_id == req_id {
                        let data_ptr = (&sod.dw_data as *const DWord) as *const f64;
                        let values = std::slice::from_raw_parts(data_ptr, 5);

                        if last_print.elapsed() >= Duration::from_millis(100) {
                            print!("\rHandle(Pos): {:.3} | Handle(%): {:3.0}% | Left(%): {:3.0}% | Right(%): {:3.0}% | Spoilers(Pos): {:.3}", 
                                values[0], values[1], values[2], values[3], values[4]
                            );
                            last_print = Instant::now();
                        }
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    }

    unsafe { close_fn(h_sc); }
}