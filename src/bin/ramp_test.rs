use hidapi::HidApi;
use std::thread;
use std::time::Duration;

fn main() {
    // =========================================================================
    // БЛОК НАСТРОЕК ТЕСТА (Меняй эти значения для экспериментов)
    // =========================================================================
    let total_test_time_s = 15.0;   // Общее время теста в секундах (теперь можно легко менять)
    let pwm_period_s = 0.15;        // Временное окно ШИМ в секундах (твои 100 мс)
    let target_intensity: u8 = 255; // Сила каждого импульса (твои 255)
    let loop_interval_ms = 20;      // Частота опроса порта (20 мс для высокой точности)
    // =========================================================================

    println!("--- Тест программного ШИМ (PWM) для Fighter R ---");
    println!("⏱️  Общая продолжительность теста: {} секунд", total_test_time_s);
    println!("⚙️  Параметры ШИМ: Окно = {} мс | Сила импульса = {}", pwm_period_s * 1000.0, target_intensity);
    println!("⚠ ОБЯЗАТЕЛЬНО закройте основной софт и SimAppPro перед тестом!");
    
    let api = HidApi::new().unwrap();
    let device = api.open(0x4098, 0xBC2A)
        .expect("Не удалось открыть устройство. Проверь, закрыты ли другие программы!");

    let mut payload: [u8; 14] = [
        0x02, 0x0A, 0xBF, 0x00, 0x00, 0x03, 0x49, 0x00, 0, 0, 0, 0, 0, 0
    ];

    // Вычисляем общее количество шагов цикла на основе заданного времени теста
    let total_steps = (total_test_time_s * 1000.0 / loop_interval_ms as f64) as i32;
    let mut last_printed_duty = -1;

    for step in 0..total_steps {
        // Текущее время теста в секундах
        let current_time_s = (step as f64 * loop_interval_ms as f64) / 1000.0;
        
        // Текущий прогресс теста от 0.0 до 1.0 (коэффициент заполнения ШИМ)
        let progress = current_time_s / total_test_time_s;
        
        // В какой точке 150-миллисекундного окна мы сейчас находимся
        let current_phase = current_time_s % pwm_period_s;
        
        // Сколько миллисекунд мотор ДОЛЖЕН работать внутри текущего окна
        let duty_threshold = pwm_period_s * progress;
        
        // Если мы внутри рабочего интервала — включаем мотор на 255 сил, иначе — выключаем
        let intensity = if current_phase < duty_threshold {
            target_intensity
        } else {
            0
        };
        
        payload[8] = intensity;
        let _ = device.write(&payload);
        
        // Выводим в консоль прогресс с шагом в 10%
        let duty_percent = (progress * 100.0) as i32;
        if duty_percent % 10 == 0 && duty_percent != last_printed_duty {
            let active_ms = (pwm_period_s * progress * 1000.0) as i32;
            let pause_ms = ((pwm_period_s * (1.0 - progress)) * 1000.0) as i32;
            
            println!(
                "[{:4.1}с / {:4.1}с] ШИМ: {:3}% | Работа: {:3} мс | Пауза: {:3} мс", 
                current_time_s,
                total_test_time_s,
                duty_percent, 
                active_ms,
                pause_ms
            );
            last_printed_duty = duty_percent;
        }
        
        thread::sleep(Duration::from_millis(loop_interval_ms));
    }

    // Полностью глушим моторы в конце теста
    payload[8] = 0;
    let _ = device.write(&payload);
    println!("--- Тест успешно завершен. Моторы выключены ---");
}