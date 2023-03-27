use std::{collections::VecDeque, time::Duration};

pub struct SlidingWindowAverage<T> {
    history_buffer: VecDeque<T>,
    max_history_size: usize,
}

impl<T> SlidingWindowAverage<T> {
    pub fn new(initial_value: T, max_history_size: usize) -> Self {
        Self {
            history_buffer: [initial_value].into_iter().collect(),
            max_history_size,
        }
    }

    pub fn submit_sample(&mut self, sample: T) {
        if self.history_buffer.len() >= self.max_history_size {
            self.history_buffer.pop_front();
        }

        self.history_buffer.push_back(sample);
    }
}

impl SlidingWindowAverage<u64> {
    pub fn get_average(&self) -> u64 {
        if !self.history_buffer.is_empty() {
            self.history_buffer.iter().sum::<u64>() / self.history_buffer.len() as u64
        } else {
            self.initial_value
        }
    }
}

impl SlidingWindowAverage<f32> {
    pub fn get_average(&self) -> f32 {
        if !self.history_buffer.is_empty() {
            self.history_buffer.iter().sum::<f32>() / self.history_buffer.len() as f32
        } else {
            self.initial_value
        }
    }
}

impl SlidingWindowAverage<usize> {
    pub fn get_average(&self) -> usize {
        if !self.history_buffer.is_empty() {
            self.history_buffer.iter().sum::<usize>() / self.history_buffer.len()
        } else {
            self.initial_value
        }
        self.history_buffer.iter().sum::<f32>() / self.history_buffer.len() as f32
    }
}

impl SlidingWindowAverage<Duration> {
    pub fn get_average(&self) -> Duration {
        self.history_buffer.iter().sum::<Duration>() / self.history_buffer.len() as u32
    }
}
