// Copyright 2026 Example. Licensed under MIT.
using System;
using System.Collections.Generic;
using static System.Math;
using Json = System.Text.Json.JsonSerializer;

namespace Acme.Jobs
{
    /// <summary>Keeps jobs in memory.</summary>
    public partial class JobStore<T> : BaseStore, IStore where T : Job
    {
        public const int MaxJobs = 100, MinJobs = 1;
        private readonly List<T> jobs = new List<T>();

        public int Count { get; private set; }

        public int Total => jobs.Count;

        public JobStore(int capacity) : base(capacity)
        {
            Reset();
        }

        public R Load<R>(string key, Func<T, R> mapper)
        {
            Validate(key);
            var job = this.Find(key);
            var text = Json.Serialize(job);
            Console.WriteLine(Max(1, 2));
            new Audit(text).Record();
            base.Flush();
            return mapper(job);
        }

        private void Validate(string key)
        {
            if (string.IsNullOrEmpty(key)) throw new ArgumentException(nameof(key));
        }

        internal T Find(string key) => jobs[0];

        partial void Reset();

        private class Audit
        {
            private readonly string text;
            public Audit(string text) { this.text = text; }
            public void Record() => Console.WriteLine(text);
        }

        public enum State { New, Done }

        public interface IListener { void Changed(string key); }

        public static void Main(string[] args)
        {
            new JobStore<Job>(1).Load("a", job => job);
        }
    }

    namespace Internal
    {
        public struct Point { public int X; }
        public record Entry(string Key);
        public record struct Size(int Width);
        public delegate void Handler(string key);
    }
}
