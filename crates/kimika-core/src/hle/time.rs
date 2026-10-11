//! the clocks: the process's own, counted from its start, and the date,
//! from the host's, in UTC, which the console's local time is too.

use std::time::{SystemTime, UNIX_EPOCH};

use super::{Call, Function};
use crate::memory::Memory;

pub const FUNCTIONS: &[(&str, Function)] = &[
    ("sceKernelGetProcessTimeLow", get_process_time_low),
    ("sceKernelGetProcessTimeWide", get_process_time_wide),
    ("sceKernelGetProcessTime", get_process_time),
    ("sceKernelGetSystemTimeLow", get_process_time_low),
    ("sceKernelGetSystemTimeWide", get_process_time_wide),
    ("sceKernelLibcGettimeofday", gettimeofday),
    ("sceKernelLibcTime", time),
    ("sceRtcGetCurrentTick", get_current_tick),
    ("sceRtcGetCurrentNetworkTick", get_current_tick),
    ("sceRtcGetTickResolution", get_tick_resolution),
    ("sceRtcGetCurrentClock", get_current_clock),
    ("sceRtcGetCurrentClockLocalTime", get_current_clock_local_time),
    ("sceRtcGetCurrentClockUtc", get_current_clock_local_time),
    ("sceRtcGetTime_t", get_time_t),
    ("sceRtcSetTime_t", set_time_t),
    ("sceRtcGetTick", get_tick),
    ("sceRtcSetTick", set_tick),
    ("sceRtcConvertUtcToLocalTime", convert_tick),
    ("sceRtcConvertLocalTimeToUtc", convert_tick),
    ("sceRtcGetDaysInMonth", get_days_in_month),
    ("sceRtcIsLeapYear", is_leap_year),
    ("sceRtcGetDayOfWeek", get_day_of_week),
];

/// microseconds from the year 1 to 1970, where the RTC's ticks start.
const RTC_EPOCH: u64 = 62_135_596_800_000_000;

fn microseconds(call: &Call) -> u64 {
    call.shared.started.elapsed().as_micros() as u64
}

fn since_1970() -> std::time::Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// sceKernelGetProcessTimeLow() -> microseconds, the low word.
fn get_process_time_low(call: &mut Call) {
    call.ret(microseconds(call) as u32);
}

/// sceKernelGetProcessTimeWide() -> microseconds
fn get_process_time_wide(call: &mut Call) {
    call.ret(microseconds(call));
}

/// sceKernelGetProcessTime(&clock)
fn get_process_time(call: &mut Call) {
    let pointer = call.u32();
    let now = microseconds(call);
    call.memory().write32(pointer, now as u32);
    call.memory().write32(pointer + 4, (now >> 32) as u32);
    call.ret(0u32);
}

/// sceKernelLibcGettimeofday(&timeval, &timezone), seconds and
/// microseconds since 1970, in UTC.
fn gettimeofday(call: &mut Call) {
    let (timeval, timezone) = (call.u32(), call.u32());
    let now = since_1970();
    let memory = call.memory();
    if timeval != 0 {
        memory.write32(timeval, now.as_secs() as u32);
        memory.write32(timeval + 4, now.subsec_micros());
    }
    if timezone != 0 {
        memory.write32(timezone, 0);
        memory.write32(timezone + 4, 0);
    }
    call.ret(0u32);
}

/// sceKernelLibcTime(&time) -> seconds since 1970
fn time(call: &mut Call) {
    let pointer = call.u32();
    let seconds = since_1970().as_secs() as u32;
    if pointer != 0 {
        call.memory().write32(pointer, seconds);
    }
    call.ret(seconds);
}

/// sceRtcGetCurrentTick(&tick), microseconds since the year 1, in UTC.
fn get_current_tick(call: &mut Call) {
    let pointer = call.u32();
    let tick = RTC_EPOCH + since_1970().as_micros() as u64;
    call.memory().write32(pointer, tick as u32);
    call.memory().write32(pointer + 4, (tick >> 32) as u32);
    call.ret(0u32);
}

/// sceRtcGetTickResolution() -> ticks a second
fn get_tick_resolution(call: &mut Call) {
    call.ret(1_000_000u32);
}

/// the year, month and day a count of days since 1970-01-01 falls on, in
/// the proleptic Gregorian calendar.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// the days since 1970-01-01 a date is, the other way around.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// writes a SceDateTime of a time in microseconds since 1970.
pub fn write_date_time(memory: &Memory, address: u32, microseconds: i64) {
    let seconds = microseconds.div_euclid(1_000_000);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let in_day = seconds.rem_euclid(86_400);
    let fields = [year, month as i64, day as i64, in_day / 3600, in_day / 60 % 60, in_day % 60];
    for (index, field) in fields.into_iter().enumerate() {
        memory.write16(address + index as u32 * 2, field as u16);
    }
    memory.write32(address + 12, microseconds.rem_euclid(1_000_000) as u32);
}

/// reads a SceDateTime as microseconds since 1970.
pub fn read_date_time(memory: &Memory, address: u32) -> i64 {
    let field = |index: u32| memory.read16(address + index * 2) as i64;
    let days = days_from_civil(field(0), field(1) as u32, field(2) as u32);
    let seconds = days * 86_400 + field(3) * 3600 + field(4) * 60 + field(5);
    seconds * 1_000_000 + memory.read32(address + 12) as i64
}

fn read_u64(memory: &Memory, address: u32) -> u64 {
    memory.read32(address) as u64 | (memory.read32(address + 4) as u64) << 32
}

fn write_u64(memory: &Memory, address: u32, value: u64) {
    memory.write32(address, value as u32);
    memory.write32(address + 4, (value >> 32) as u32);
}

/// sceRtcGetCurrentClock(&date, minutes east of UTC)
fn get_current_clock(call: &mut Call) {
    let (date, offset) = (call.u32(), call.i32());
    let now = since_1970().as_micros() as i64 + offset as i64 * 60_000_000;
    write_date_time(call.memory(), date, now);
    call.ret(0u32);
}

/// sceRtcGetCurrentClockLocalTime(&date)
fn get_current_clock_local_time(call: &mut Call) {
    let date = call.u32();
    write_date_time(call.memory(), date, since_1970().as_micros() as i64);
    call.ret(0u32);
}

/// sceRtcGetTime_t(&date, &seconds since 1970)
fn get_time_t(call: &mut Call) {
    let (date, time) = (call.u32(), call.u32());
    let seconds = read_date_time(call.memory(), date).div_euclid(1_000_000);
    call.memory().write32(time, seconds as u32);
    call.ret(0u32);
}

/// sceRtcSetTime_t(&date, seconds since 1970)
fn set_time_t(call: &mut Call) {
    let (date, time) = (call.u32(), call.i32());
    write_date_time(call.memory(), date, time as i64 * 1_000_000);
    call.ret(0u32);
}

/// sceRtcGetTick(&date, &tick)
fn get_tick(call: &mut Call) {
    let (date, tick) = (call.u32(), call.u32());
    let value = read_date_time(call.memory(), date) + RTC_EPOCH as i64;
    write_u64(call.memory(), tick, value as u64);
    call.ret(0u32);
}

/// sceRtcSetTick(&date, &tick)
fn set_tick(call: &mut Call) {
    let (date, tick) = (call.u32(), call.u32());
    let value = read_u64(call.memory(), tick) as i64 - RTC_EPOCH as i64;
    write_date_time(call.memory(), date, value);
    call.ret(0u32);
}

/// sceRtcConvertUtcToLocalTime(&from, &to) and the other way: the same
/// tick, the console's zone being UTC here.
fn convert_tick(call: &mut Call) {
    let (from, to) = (call.u32(), call.u32());
    let value = read_u64(call.memory(), from);
    write_u64(call.memory(), to, value);
    call.ret(0u32);
}

fn leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// sceRtcGetDaysInMonth(year, month)
fn get_days_in_month(call: &mut Call) {
    let (year, month) = (call.i32() as i64, call.u32());
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        2 => 28,
        _ => return call.ret(super::super::kernel::errors::ILLEGAL_ADDR),
    };
    call.ret(days as u32);
}

/// sceRtcIsLeapYear(year)
fn is_leap_year(call: &mut Call) {
    let year = call.i32() as i64;
    call.ret(leap(year) as u32);
}

/// sceRtcGetDayOfWeek(year, month, day) -> 0 for Sunday
fn get_day_of_week(call: &mut Call) {
    let (year, month, day) = (call.i32() as i64, call.u32(), call.u32());
    // 1970-01-01 was a Thursday
    let weekday = (days_from_civil(year, month, day) + 4).rem_euclid(7);
    call.ret(weekday as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_dates_turn_into_each_other() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_737), (2026, 10, 11));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        for days in [-800_000, -1, 0, 59, 11_016, 20_737, 2_000_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
    }

    #[test]
    fn a_date_time_reads_back_as_written() {
        let memory = Memory::new();
        assert!(memory.commit(0x8100_0000, 0x1000));
        let moment = 1_791_725_445_123_456;
        write_date_time(&memory, 0x8100_0000, moment);
        assert_eq!(memory.read16(0x8100_0000), 2026);
        assert_eq!(read_date_time(&memory, 0x8100_0000), moment);
    }
}
